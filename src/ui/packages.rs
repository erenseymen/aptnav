//! The package list screen: browses whatever context (a repo, "All
//! packages", or "Not from any repository") was opened from the repo
//! screen, with a detail pane for the selected package.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use ratatui::Terminal;

use crate::apt::model::InstallState;

use super::app::{App, PendingAction, PendingConfirm, Screen};
use super::overlay::Confirm;

pub const HELP: &[(&str, &str)] = &[
    ("↑↓ / jk", "move selection"),
    ("/", "filter packages (fuzzy)"),
    (
        "Tab",
        "cycle scope: all / installed / upgradable / not installed",
    ),
    ("i", "install the selected package"),
    ("r", "remove the selected package"),
    ("p", "purge the selected package (remove + config files)"),
    ("U", "upgrade the selected package"),
    ("M", "toggle auto/manual-installed marking"),
    (
        "v",
        "pick a specific version to install, across all sources",
    ),
    ("Esc", "back to repositories"),
];

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let Some(pkgs) = &app.packages else { return };

    let top = if pkgs.filter_active || !pkgs.filter_text.is_empty() {
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
    if top.len() == 2 {
        frame.render_widget(Paragraph::new(format!("/{}", pkgs.filter_text)), top[0]);
        idx = 1;
    }

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(top[idx]);

    let items: Vec<ListItem> = pkgs
        .filtered
        .iter()
        .map(|&i| {
            let row = &pkgs.all_rows[i];
            let state_marker = match row.install_state {
                InstallState::Installed if app.data.is_upgradable(&row.name) => {
                    Span::styled("\u{2191}", Style::default().fg(Color::Yellow))
                }
                InstallState::Installed => Span::styled("i", Style::default().fg(Color::Green)),
                InstallState::ConfigFiles => {
                    Span::styled("c", Style::default().fg(Color::DarkGray))
                }
                InstallState::Broken => Span::styled("!", Style::default().fg(Color::Red)),
                InstallState::NotInstalled => Span::raw(" "),
            };
            ListItem::new(Line::from(vec![
                state_marker,
                Span::raw(" "),
                Span::raw(row.name.clone()),
                Span::styled(
                    format!("  {}", row.version),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    if !pkgs.filtered.is_empty() {
        state.select(Some(pkgs.selected));
    }
    let title = format!(
        "{} \u{2014} {} [{}]",
        pkgs.context_label,
        pkgs.filtered.len(),
        pkgs.scope.label()
    );
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, body[0], &mut state);

    let detail = render_detail(app, pkgs);
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(Block::default().title("Details").borders(Borders::ALL)),
        body[1],
    );
}

fn render_detail(app: &App, pkgs: &super::app::PackageListState) -> Vec<Line<'static>> {
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
    if app.data.is_upgradable(&row.name) {
        lines.push(Line::from(Span::styled(
            "Upgrade available",
            Style::default().fg(Color::Yellow),
        )));
    }
    lines.push(Line::from(""));

    if let Some(fields) = app.data.full_stanza_text(row.stub) {
        for (k, v) in fields {
            if k == "Package" || k == "Version" {
                continue;
            }
            lines.push(Line::from(vec![
                Span::styled(format!("{k}: "), Style::default().fg(Color::Cyan)),
                Span::raw(v),
            ]));
        }
    } else if let Some(installed) = app.data.dpkg.packages.get(&row.name) {
        for (k, v) in &installed.fields {
            if k == "Package" || k == "Version" || k == "Status" {
                continue;
            }
            lines.push(Line::from(vec![
                Span::styled(format!("{k}: "), Style::default().fg(Color::Cyan)),
                Span::raw(v.clone()),
            ]));
        }
    }
    lines
}

pub fn handle_key<B: Backend + std::io::Write>(
    app: &mut App,
    key: KeyEvent,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    let filter_active = app
        .packages
        .as_ref()
        .map(|p| p.filter_active)
        .unwrap_or(false);
    if filter_active {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                if let Some(p) = &mut app.packages {
                    p.filter_active = false;
                }
            }
            KeyCode::Backspace => {
                if let Some(p) = &mut app.packages {
                    p.filter_text.pop();
                }
                app.refresh_package_filter();
            }
            KeyCode::Char(c) => {
                if let Some(p) = &mut app.packages {
                    p.filter_text.push(c);
                }
                app.refresh_package_filter();
            }
            _ => {}
        }
        return Ok(());
    }

    match key.code {
        KeyCode::Esc => {
            app.screen = Screen::Repos;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(p) = &mut app.packages {
                p.selected = p.selected.saturating_sub(1);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(p) = &mut app.packages {
                if p.selected + 1 < p.filtered.len() {
                    p.selected += 1;
                }
            }
        }
        KeyCode::Char('/') => {
            if let Some(p) = &mut app.packages {
                p.filter_active = true;
            }
        }
        KeyCode::Tab => {
            if let Some(p) = &mut app.packages {
                p.scope = p.scope.next();
            }
            app.refresh_package_filter();
        }
        KeyCode::Char('i') => queue_dry_run(app, "install"),
        KeyCode::Char('r') => queue_dry_run(app, "remove"),
        KeyCode::Char('p') => queue_dry_run(app, "purge"),
        KeyCode::Char('U') => queue_dry_run(app, "upgrade"),
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
                Span::styled("installed", Style::default().fg(Color::Green))
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
                    Style::default().fg(Color::DarkGray),
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
    let pkgs = app.packages.as_ref()?;
    let &i = pkgs.filtered.get(pkgs.selected)?;
    Some(pkgs.all_rows[i].name.clone())
}

fn queue_dry_run(app: &mut App, action: &str) {
    let Some(name) = selected_name(app) else {
        return;
    };
    let apt_action = match action {
        "upgrade" => "install",
        other => other,
    };
    match crate::actions::apt::dry_run(apt_action, &[name.clone()]) {
        Ok(plan) => {
            let mut detail = Vec::new();
            if !plan.to_install.is_empty() {
                detail.push(format!("Install: {}", plan.to_install.join(", ")));
            }
            if !plan.to_upgrade.is_empty() {
                detail.push(format!("Upgrade: {}", plan.to_upgrade.join(", ")));
            }
            if !plan.to_remove.is_empty() {
                detail.push(format!("Remove: {}", plan.to_remove.join(", ")));
            }
            if !plan.held_back.is_empty() {
                detail.push(format!("Held back: {}", plan.held_back.join(", ")));
            }
            if let Some(d) = &plan.disk_delta {
                detail.push(d.clone());
            }
            if detail.is_empty() {
                detail.push("No changes reported by the simulation.".to_string());
            }
            let pending_action = match action {
                "install" | "upgrade" => PendingAction::InstallPackages(vec![name.clone()]),
                "remove" => PendingAction::RemovePackages(vec![name.clone()]),
                "purge" => PendingAction::PurgePackages(vec![name.clone()]),
                _ => return,
            };
            app.confirm = Some(PendingConfirm {
                ui: Confirm {
                    title: format!("{action} {name}"),
                    detail,
                    prompt: format!("Proceed with `apt-get {apt_action} {name}`?"),
                },
                action: pending_action,
            });
        }
        Err(e) => app.set_status(format!("Simulation failed: {e}"), true),
    }
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
