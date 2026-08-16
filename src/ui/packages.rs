//! Package list rendering and key handling. Shared by two call sites: the
//! Packages tab (always "All packages") and the Repos tab's Drill mode (one
//! repository's packages). Neither owns a `PackageListState` directly here —
//! callers pass one in for `draw`, and `handle_key` resolves "whichever one
//! is currently on screen" via [`App::active_packages_mut`].

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::Frame;
use ratatui::Terminal;

use crate::apt::model::InstallState;
use crate::apt::PackageRow;

use super::app::{App, PackageListState, PendingAction, PendingConfirm};
use super::overlay::Confirm;
use super::sort::SortColumn;

pub const HELP: &[(&str, &str)] = &[
    ("↑↓ / jk", "move cursor"),
    ("/", "filter packages (fuzzy)"),
    ("Space", "toggle selection of the cursor row"),
    ("a", "select every filtered package"),
    ("A", "deselect every filtered package"),
    ("Ctrl-A", "clear the entire selection"),
    ("s", "cycle scope: all / installed / upgradable / not installed"),
    ("S", "cycle column sort: name / version / state, each asc/desc"),
    ("i", "install (selection, else cursor row)"),
    ("r", "remove (selection, else cursor row)"),
    ("p", "purge (selection, else cursor row; + config files)"),
    ("U", "upgrade (selection, else cursor row)"),
    ("H", "toggle hold / unhold (cursor row)"),
    ("M", "toggle auto/manual-installed marking (cursor row)"),
    (
        "v",
        "pick a specific version to install (cursor row), across all sources",
    ),
    ("q", "quit"),
];

/// Marker column: checkbox, install-state icon, hold/essential badges — the
/// one non-sortable column, so it isn't included in `COL_CONSTRAINTS`'s
/// sortable set.
fn marker_spans(app: &App, row: &PackageRow) -> Vec<Span<'static>> {
    let checkbox = if app.selection.contains(&row.name) {
        Span::styled("[x]", Style::default().fg(app.theme.badge_selected))
    } else {
        Span::raw("[ ]")
    };
    let state_marker = match row.install_state {
        InstallState::Installed if row.upgradable => {
            Span::styled("\u{2191}", Style::default().fg(app.theme.warn))
        }
        InstallState::Installed => Span::styled("i", Style::default().fg(app.theme.ok)),
        InstallState::ConfigFiles => Span::styled("c", Style::default().fg(app.theme.dim)),
        InstallState::Broken => Span::styled("!", Style::default().fg(app.theme.error)),
        InstallState::NotInstalled => Span::raw(" "),
    };
    let mut spans = vec![checkbox, Span::raw(" "), state_marker];
    if row.held {
        spans.push(Span::styled(
            " \u{229d}",
            Style::default().fg(app.theme.badge_hold),
        ));
    }
    if row.essential {
        spans.push(Span::styled(
            " \u{25c8}",
            Style::default().fg(app.theme.badge_essential),
        ));
    }
    spans
}

fn state_label(row: &PackageRow) -> &'static str {
    match row.install_state {
        InstallState::Installed if row.upgradable => "upgradable",
        InstallState::Installed => "installed",
        InstallState::ConfigFiles => "config files",
        InstallState::Broken => "broken",
        InstallState::NotInstalled => "not installed",
    }
}

/// Column widths for the package table, shared between the `Table` widget
/// itself and the header hit-rect computation below it — the same array fed
/// to both is what keeps them from drifting apart (see [`super::layout`]).
const COL_CONSTRAINTS: [Constraint; 4] = [
    Constraint::Length(9),
    Constraint::Percentage(40),
    Constraint::Percentage(25),
    Constraint::Percentage(20),
];

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let pkgs = app.active_packages();
    let filter_line = if pkgs.filter_active || !pkgs.filter_text.is_empty() {
        Some(format!("/{}", pkgs.filter_text))
    } else {
        None
    };

    let top = if filter_line.is_some() {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(3)])
            .split(area)
    } else {
        Layout::default()
            .constraints([Constraint::Min(3)])
            .split(area)
    };
    let mut idx = 0;
    if let Some(line) = &filter_line {
        frame.render_widget(Paragraph::new(line.as_str()), top[0]);
        idx = 1;
    }

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(top[idx]);
    let list_area = body[0];
    let detail_area = body[1];

    let pkgs = app.active_packages();
    let rows: Vec<Row> = pkgs
        .filtered
        .iter()
        .map(|&i| {
            let row = &pkgs.all_rows[i];
            let name_style = if row.held {
                Style::default().fg(app.theme.dim)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(Line::from(marker_spans(app, row))),
                Cell::from(Span::styled(row.name.clone(), name_style)),
                Cell::from(Span::styled(
                    row.version.clone(),
                    Style::default().fg(app.theme.dim),
                )),
                Cell::from(state_label(row)),
            ])
        })
        .collect();
    let title = if app.selection.is_empty() {
        format!(
            "{} \u{2014} {} [{}]",
            pkgs.context_label,
            pkgs.filtered.len(),
            pkgs.scope.label()
        )
    } else {
        format!(
            "{} \u{2014} {} [{}] \u{00b7} {} selected",
            pkgs.context_label,
            pkgs.filtered.len(),
            pkgs.scope.label(),
            app.selection.len()
        )
    };
    let selected = pkgs.selected;
    let has_rows = !pkgs.filtered.is_empty();

    let header = Row::new(vec![
        Cell::from(""),
        Cell::from(format!("Name{}", app.sort.indicator(SortColumn::Name))),
        Cell::from(format!("Version{}", app.sort.indicator(SortColumn::Version))),
        Cell::from(format!("State{}", app.sort.indicator(SortColumn::State))),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(list_area);

    let table = Table::new(rows, COL_CONSTRAINTS)
        .header(header)
        .block(block)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut state = TableState::default();
    if has_rows {
        state.select(Some(selected));
    }
    frame.render_stateful_widget(table, list_area, &mut state);

    // Column hit-rects for mouse click-to-sort: split the header row with
    // the *same* constraints the table itself uses, so this can never
    // drift out of sync with what was drawn.
    app.layout.pkg_checkbox_width = 3; // "[x]" / "[ ]"
    if inner.height > 0 {
        let header_rect = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        };
        let col_rects = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(COL_CONSTRAINTS)
            .split(header_rect);
        app.layout.pkg_col_hits = vec![
            (col_rects[1], SortColumn::Name),
            (col_rects[2], SortColumn::Version),
            (col_rects[3], SortColumn::State),
        ];
        app.layout.pkg_rows = Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: inner.height.saturating_sub(1),
        };
    } else {
        app.layout.pkg_col_hits.clear();
        app.layout.pkg_rows = Rect::default();
    }
    app.layout.pkg_row_offset = state.offset();

    let pkgs = app.active_packages();
    let detail = render_detail(app, pkgs);
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(Block::default().title("Details").borders(Borders::ALL)),
        detail_area,
    );
}

fn render_detail(app: &App, pkgs: &PackageListState) -> Vec<Line<'static>> {
    let Some(&i) = pkgs.filtered.get(pkgs.selected) else {
        return vec![Line::from("No package selected")];
    };
    let row = &pkgs.all_rows[i];
    let mut lines = vec![
        Line::from(Span::styled(
            row.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("Version: {}", row.version)),
        Line::from(format!(
            "State: {}",
            match row.install_state {
                InstallState::Installed => "installed",
                InstallState::ConfigFiles => "removed (config files remain)",
                InstallState::Broken => "broken / half-configured",
                InstallState::NotInstalled => "not installed",
            }
        )),
    ];
    if row.install_state.is_present() {
        lines.push(Line::from(format!(
            "Auto-installed: {}",
            if row.auto_installed {
                "yes (dependency)"
            } else {
                "no (explicit)"
            }
        )));
    }
    if row.upgradable {
        lines.push(Line::from(Span::styled(
            "Upgrade available",
            Style::default().fg(app.theme.warn),
        )));
    }
    if row.held {
        lines.push(Line::from(Span::styled(
            "Held (apt-mark hold) \u{2014} press H to unhold",
            Style::default().fg(app.theme.badge_hold),
        )));
    }
    if row.essential {
        lines.push(Line::from(Span::styled(
            "Essential \u{2014} cannot be removed or purged",
            Style::default().fg(app.theme.badge_essential),
        )));
    }
    if let Some(pct) = row.phased_pct {
        lines.push(Line::from(Span::styled(
            format!("Phased rollout: {pct}% of machines so far"),
            Style::default().fg(app.theme.warn),
        )));
    }
    lines.push(Line::from(""));

    if let Some(fields) = app.data.full_stanza_text(row.stub) {
        for (k, v) in fields {
            if k == "Package" || k == "Version" {
                continue;
            }
            lines.push(Line::from(vec![
                Span::styled(format!("{k}: "), Style::default().fg(app.theme.accent)),
                Span::raw(v),
            ]));
        }
    } else if let Some(installed) = app.data.dpkg.packages.get(&row.name) {
        for (k, v) in &installed.fields {
            if k == "Package" || k == "Version" || k == "Status" {
                continue;
            }
            lines.push(Line::from(vec![
                Span::styled(format!("{k}: "), Style::default().fg(app.theme.accent)),
                Span::raw(v.clone()),
            ]));
        }
    }
    lines
}

/// Key handling for whichever `PackageListState` is currently active (see
/// [`App::active_packages_mut`]) — called directly as the Packages tab's
/// handler, and by `repos::handle_key` when the Repos tab is in Drill mode.
pub fn handle_key<B: Backend + std::io::Write>(
    app: &mut App,
    key: KeyEvent,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    let filter_active = app.active_packages().filter_active;
    if filter_active {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                app.active_packages_mut().filter_active = false;
            }
            KeyCode::Backspace => {
                app.active_packages_mut().filter_text.pop();
                app.refresh_active_package_filter();
            }
            KeyCode::Char(c) => {
                app.active_packages_mut().filter_text.push(c);
                app.refresh_active_package_filter();
            }
            _ => {}
        }
        return Ok(());
    }

    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Esc => {
            // No "back" to go to any more (tabs are peers, not a stack); a
            // plain Esc with no filter open is a no-op.
        }
        KeyCode::Up | KeyCode::Char('k') => {
            let pkgs = app.active_packages_mut();
            pkgs.selected = pkgs.selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let pkgs = app.active_packages_mut();
            if pkgs.selected + 1 < pkgs.filtered.len() {
                pkgs.selected += 1;
            }
        }
        KeyCode::Char('/') => {
            app.active_packages_mut().filter_active = true;
        }
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.selection.clear();
        }
        KeyCode::Char(' ') => {
            if let Some(name) = selected_name(app) {
                app.selection.toggle(&name);
            }
        }
        KeyCode::Char('a') => {
            let names = filtered_names(app);
            let added = app.selection.insert_all(names.iter().map(String::as_str));
            app.set_status(format!("Selected {added} more package(s)"), false);
        }
        KeyCode::Char('A') => {
            let names = filtered_names(app);
            let removed = app.selection.remove_all(names.iter().map(String::as_str));
            app.set_status(format!("Deselected {removed} package(s)"), false);
        }
        KeyCode::Char('s') => {
            let pkgs = app.active_packages_mut();
            pkgs.scope = pkgs.scope.next();
            app.refresh_active_package_filter();
        }
        KeyCode::Char('S') => {
            app.sort.cycle_ring();
            app.refresh_active_package_filter();
        }
        KeyCode::Char('i') => queue_dry_run(app, "install"),
        KeyCode::Char('r') => queue_dry_run(app, "remove"),
        KeyCode::Char('p') => queue_dry_run(app, "purge"),
        KeyCode::Char('U') => queue_dry_run(app, "upgrade"),
        KeyCode::Char('H') => toggle_hold(app, terminal)?,
        KeyCode::Char('M') => toggle_auto_manual(app, terminal)?,
        KeyCode::Char('v') => open_version_picker(app),
        _ => {}
    }
    Ok(())
}

/// Fetch every version of the selected package across all repositories (via
/// `apt-cache policy`) and open the `v` overlay.
fn open_version_picker(app: &mut App) {
    let Some(name) = selected_name(app) else {
        return;
    };
    match crate::apt::policy::policy_for(&name) {
        Ok(policy) => {
            if policy.versions.len() <= 1 {
                app.set_status(format!("{name} is only available from one source"), false);
                return;
            }
            let selected = policy
                .versions
                .iter()
                .position(|v| v.is_installed)
                .unwrap_or(0);
            app.version_picker = Some(super::app::VersionPickerState {
                package: name,
                policy,
                selected,
            });
        }
        Err(e) => app.set_status(format!("Could not query versions: {e}"), true),
    }
}

pub fn render_version_picker(frame: &mut ratatui::Frame, app: &App) {
    let Some(vp) = &app.version_picker else {
        return;
    };
    let area = super::overlay::centered_rect(70, 60, frame.area());
    frame.render_widget(ratatui::widgets::Clear, area);

    let items: Vec<ListItem> = vp
        .policy
        .versions
        .iter()
        .map(|v| {
            let origin = v
                .sources
                .first()
                .map(|(_, d)| d.as_str())
                .unwrap_or("(local)");
            let prio = v.sources.first().map(|(p, _)| *p).unwrap_or(0);
            let marker = if v.is_installed {
                Span::styled("installed", Style::default().fg(app.theme.ok))
            } else {
                Span::raw("")
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<28}", v.version),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" prio {prio:<4} "),
                    Style::default().fg(app.theme.dim),
                ),
                Span::raw(origin),
                Span::raw("  "),
                marker,
            ]))
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(vp.selected));
    let list = List::new(items)
        .block(
            Block::default()
                .title(format!(
                    "Versions of {} (Enter to install, Esc to close)",
                    vp.package
                ))
                .borders(Borders::ALL),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, area, &mut state);
}

pub fn handle_version_picker_key<B: Backend + std::io::Write>(
    app: &mut App,
    key: KeyEvent,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    match key.code {
        KeyCode::Esc => app.version_picker = None,
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(vp) = &mut app.version_picker {
                vp.selected = vp.selected.saturating_sub(1);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(vp) = &mut app.version_picker {
                if vp.selected + 1 < vp.policy.versions.len() {
                    vp.selected += 1;
                }
            }
        }
        KeyCode::Enter => install_selected_version(app, terminal)?,
        _ => {}
    }
    Ok(())
}

/// Install a specific version via `apt-get install pkg=version`, the
/// standard way to pin an install to one of several available versions.
fn install_selected_version<B: Backend + std::io::Write>(
    app: &mut App,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    let Some(vp) = app.version_picker.take() else {
        return Ok(());
    };
    let Some(chosen) = vp.policy.versions.get(vp.selected) else {
        return Ok(());
    };
    if chosen.is_installed {
        app.set_status(
            format!("{} {} is already installed", vp.package, chosen.version),
            false,
        );
        return Ok(());
    }
    let target = format!("{}={}", vp.package, chosen.version);

    app.with_suspended_terminal(terminal, |_| Ok(()))?;
    if let Err(e) = app.privileges.ensure() {
        app.set_status(format!("Authentication failed: {e}"), true);
        return Ok(());
    }
    match crate::actions::apt::run_action("install", &[target.clone()]) {
        Ok(logged) if logged.success => {
            app.set_status(format!("Installed {target}"), false);
        }
        Ok(_) => app.set_status(format!("Installing {target} failed (see log)"), true),
        Err(e) => app.set_status(format!("{e}"), true),
    }
    let _ = app.reload_data();
    Ok(())
}

fn selected_name(app: &App) -> Option<String> {
    let pkgs = app.active_packages();
    let &i = pkgs.filtered.get(pkgs.selected)?;
    Some(pkgs.all_rows[i].name.clone())
}

/// Every name currently in the filtered view of the active package list —
/// what `a`/`A` (select/deselect filtered) operate on.
fn filtered_names(app: &App) -> Vec<String> {
    let pkgs = app.active_packages();
    pkgs.filtered
        .iter()
        .map(|&i| pkgs.all_rows[i].name.clone())
        .collect()
}

/// The input to every mutating action: the selection if one exists,
/// otherwise just the cursor row. Deliberately does *not* intersect the
/// selection with what's currently filtered/visible — see `App::selection`'s
/// doc comment for why a silent intersection would be the wrong call here.
fn action_targets(app: &App) -> Vec<String> {
    if !app.selection.is_empty() {
        return app.selection.names().to_vec();
    }
    selected_name(app).into_iter().collect()
}

/// Split `names` into (blocked, allowed) by whether each is essential.
/// Essential packages can never be removed/purged from here — callers
/// proceed with `allowed` and mention `blocked` in the confirm detail (or,
/// if `allowed` ends up empty, refuse the action outright rather than
/// running a dry-run on nothing).
fn partition_essential(names: &[String], data: &crate::apt::AptData) -> (Vec<String>, Vec<String>) {
    let mut blocked = Vec::new();
    let mut allowed = Vec::new();
    for name in names {
        if data.is_essential(name) {
            blocked.push(name.clone());
        } else {
            allowed.push(name.clone());
        }
    }
    (blocked, allowed)
}

/// Label a `held_back` name with why apt-get kept it back: an explicit
/// `apt-mark hold` wins over phasing (a package can be both, and `hold` is
/// the more actionable of the two — `H` fixes it, phasing just needs
/// waiting or forcing), a phased rollout gets its percentage, and anything
/// else is presumed to be a dependency hold-back.
fn phased_label(name: &str, data: &crate::apt::AptData) -> String {
    if data.is_held(name) {
        format!("{name} (held)")
    } else if let Some(pct) = data.phased_pct(name) {
        format!("{name} (phased {pct}%)")
    } else {
        format!("{name} (dependency)")
    }
}

fn queue_dry_run(app: &mut App, action: &str) {
    let mut names = action_targets(app);
    if names.is_empty() {
        return;
    }

    let mut detail = Vec::new();

    if matches!(action, "remove" | "purge") {
        let (blocked, allowed) = partition_essential(&names, &app.data);
        if !blocked.is_empty() {
            if allowed.is_empty() {
                app.set_status(
                    format!(
                        "Cannot {action} essential package(s): {}",
                        blocked.join(", ")
                    ),
                    true,
                );
                return;
            }
            detail.push(format!(
                "Skipped essential package(s): {}",
                blocked.join(", ")
            ));
        }
        names = allowed;
    }

    // Held packages block install/upgrade but not remove/purge.
    if matches!(action, "install" | "upgrade") {
        let (held, not_held): (Vec<String>, Vec<String>) =
            names.into_iter().partition(|n| app.data.is_held(n));
        if not_held.is_empty() {
            app.set_status(
                format!("{} held; unhold first (H)", held.join(", ")),
                true,
            );
            return;
        }
        if !held.is_empty() {
            detail.push(format!("Skipped held package(s): {}", held.join(", ")));
        }
        names = not_held;
    }

    let apt_action = match action {
        "upgrade" => "install",
        other => other,
    };
    match crate::actions::apt::dry_run(apt_action, &names) {
        Ok(plan) => {
            let visible = filtered_names(app);
            let not_visible = names.iter().filter(|n| !visible.contains(n)).count();
            if not_visible > 0 {
                detail.push(format!(
                    "{not_visible} of the selected package(s) are not visible in the current view."
                ));
            }
            if !plan.to_install.is_empty() {
                detail.push(format!("Install: {}", plan.to_install.join(", ")));
            }
            if !plan.to_upgrade.is_empty() {
                detail.push(format!("Upgrade: {}", plan.to_upgrade.join(", ")));
            }
            if !plan.to_remove.is_empty() {
                detail.push(format!("Remove: {}", plan.to_remove.join(", ")));
            }
            // Phased-update detection: `held_back` is already parsed from
            // this simulation's own output, so the labeling here costs no
            // extra apt-get call — just a stanza-field lookup per name.
            let phased_names: Vec<String> = plan
                .held_back
                .iter()
                .filter(|n| !app.data.is_held(n) && app.data.phased_pct(n).is_some())
                .cloned()
                .collect();
            if !plan.held_back.is_empty() {
                let labels: Vec<String> = plan
                    .held_back
                    .iter()
                    .map(|n| phased_label(n, &app.data))
                    .collect();
                detail.push(format!("Held back: {}", labels.join(", ")));
            }
            if let Some(d) = &plan.disk_delta {
                detail.push(d.clone());
            }
            if detail.is_empty() {
                detail.push("No changes reported by the simulation.".to_string());
            }
            let pending_action = match action {
                "install" | "upgrade" => PendingAction::InstallPackages(names.clone()),
                "remove" => PendingAction::RemovePackages(names.clone()),
                "purge" => PendingAction::PurgePackages(names.clone()),
                _ => return,
            };
            let title_subject = if names.len() == 1 {
                names[0].clone()
            } else {
                format!("{} packages", names.len())
            };
            // The prompt shows the actual command that will run, not the
            // count-summarized title — so it stays honest for a bulk action
            // instead of literally reading "apt-get install 2 packages".
            let command_preview = if names.len() <= 4 {
                names.join(" ")
            } else {
                format!(
                    "{} \u{2026} and {} more",
                    names[..3].join(" "),
                    names.len() - 3
                )
            };

            // A phased package the simulation kept back offers a third
            // choice: force it through (the primary `y` action becomes
            // "install everything, phasing included") or explicitly skip
            // just the phased ones (the alt `s` action).
            let (final_action, alt) = if matches!(action, "install" | "upgrade")
                && !phased_names.is_empty()
            {
                let skip_names: Vec<String> = names
                    .iter()
                    .filter(|n| !phased_names.contains(n))
                    .cloned()
                    .collect();
                (
                    PendingAction::InstallPackagesForced(names.clone()),
                    Some((
                        PendingAction::InstallPackages(skip_names),
                        ('s', "skip phased".to_string()),
                    )),
                )
            } else {
                (pending_action, None)
            };
            let (alt_action, alt_ui) = match alt {
                Some((a, ui)) => (Some(a), Some(ui)),
                None => (None, None),
            };

            app.confirm = Some(PendingConfirm {
                ui: Confirm {
                    title: format!("{action} {title_subject}"),
                    detail,
                    prompt: format!("Proceed with `apt-get {apt_action} {command_preview}`?"),
                    alt: alt_ui,
                },
                action: final_action,
                alt_action,
            });
        }
        Err(e) => app.set_status(format!("Simulation failed: {e}"), true),
    }
}

/// `apt-mark hold`/`unhold` the cursor row. Not selection-aware, like
/// [`toggle_auto_manual`] — hold is a per-package marker a user sets one at
/// a time, not a bulk action.
fn toggle_hold<B: Backend + std::io::Write>(
    app: &mut App,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    let Some(name) = selected_name(app) else {
        return Ok(());
    };
    let currently_held = app.data.is_held(&name);
    let mark = if currently_held { "unhold" } else { "hold" };

    app.with_suspended_terminal(terminal, |_| Ok(()))?;
    if let Err(e) = app.privileges.ensure() {
        app.set_status(format!("Authentication failed: {e}"), true);
        return Ok(());
    }

    let result = (|| -> anyhow::Result<()> {
        let status =
            crate::actions::privileges::sudo_command("apt-mark", &[mark, &name]).status()?;
        if !status.success() {
            anyhow::bail!("apt-mark {mark} failed");
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            app.set_status(format!("apt-mark {mark} {name}"), false);
            let _ = app.reload_data();
        }
        Err(e) => app.set_status(format!("{e}"), true),
    }
    Ok(())
}

fn toggle_auto_manual<B: Backend + std::io::Write>(
    app: &mut App,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    let Some(name) = selected_name(app) else {
        return Ok(());
    };
    let currently_auto = app.data.dpkg.is_auto(&name);
    let mark = if currently_auto { "manual" } else { "auto" };

    app.with_suspended_terminal(terminal, |_| Ok(()))?;
    if let Err(e) = app.privileges.ensure() {
        app.set_status(format!("Authentication failed: {e}"), true);
        return Ok(());
    }

    let result = (|| -> anyhow::Result<()> {
        let status =
            crate::actions::privileges::sudo_command("apt-mark", &[mark, &name]).status()?;
        if !status.success() {
            anyhow::bail!("apt-mark {mark} failed");
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            app.set_status(format!("Marked {name} as {mark}"), false);
            let _ = app.reload_data();
        }
        Err(e) => app.set_status(format!("{e}"), true),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apt::{AptData, AptRoots};
    use std::fs;
    use std::io::Write as _;
    use tempfile::TempDir;

    fn write(dir: &TempDir, rel: &str, content: &str) {
        let p = dir.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    fn fixture() -> TempDir {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list",
            "deb https://example.org/ubuntu/ noble main\n",
        );
        write(
            &dir,
            "var/lib/apt/lists/example.org_ubuntu_dists_noble_main_binary-amd64_Packages",
            "Package: dpkg\nVersion: 1.0\nEssential: yes\n\n\
             Package: vim\nVersion: 1.0\n\n",
        );
        write(
            &dir,
            "var/lib/dpkg/status",
            "Package: dpkg\nStatus: install ok installed\nVersion: 1.0\nEssential: yes\n\n\
             Package: vim\nStatus: install ok installed\nVersion: 1.0\n\n\
             Package: held-pkg\nStatus: hold ok installed\nVersion: 1.0\n",
        );
        dir
    }

    #[test]
    fn partition_essential_blocks_essential_only() {
        let dir = fixture();
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let names = vec!["dpkg".to_string(), "vim".to_string()];
        let (blocked, allowed) = partition_essential(&names, &data);
        assert_eq!(blocked, vec!["dpkg".to_string()]);
        assert_eq!(allowed, vec!["vim".to_string()]);
    }

    #[test]
    fn partition_essential_all_essential_leaves_allowed_empty() {
        let dir = fixture();
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let names = vec!["dpkg".to_string()];
        let (blocked, allowed) = partition_essential(&names, &data);
        assert_eq!(blocked, vec!["dpkg".to_string()]);
        assert!(allowed.is_empty());
    }

    #[test]
    fn partition_essential_none_essential_is_passthrough() {
        let dir = fixture();
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let names = vec!["vim".to_string()];
        let (blocked, allowed) = partition_essential(&names, &data);
        assert!(blocked.is_empty());
        assert_eq!(allowed, vec!["vim".to_string()]);
    }

    #[test]
    fn phased_label_prefers_held_over_phased() {
        let dir = fixture();
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        assert_eq!(phased_label("held-pkg", &data), "held-pkg (held)");
    }

    #[test]
    fn phased_label_reports_percentage_when_not_held() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list",
            "deb https://example.org/ubuntu/ noble main\n",
        );
        write(
            &dir,
            "var/lib/apt/lists/example.org_ubuntu_dists_noble_main_binary-amd64_Packages",
            "Package: rolling\nVersion: 1.0\nPhased-Update-Percentage: 40\n\n",
        );
        write(
            &dir,
            "var/lib/dpkg/status",
            "Package: rolling\nStatus: install ok installed\nVersion: 0.9\n",
        );
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        assert_eq!(phased_label("rolling", &data), "rolling (phased 40%)");
    }

    #[test]
    fn phased_label_falls_back_to_dependency() {
        let dir = fixture();
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        assert_eq!(phased_label("vim", &data), "vim (dependency)");
    }
}
