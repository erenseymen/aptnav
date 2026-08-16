//! The Repos tab: repository list plus each repository's health diagnoses
//! side by side (`List` mode), or one repository's packages drilled into
//! inline (`Drill` mode, which reuses the Packages tab's own rendering and
//! key handling — see [`super::packages`]).

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use ratatui::Terminal;

use crate::actions::edit;
use crate::health::fixes::Fix;
use crate::health::{Diagnosis, Severity};

use super::app::{
    App, PackageContext, PendingAction, PendingConfirm, PendingPrompt, RepoRow, ReposFocus,
    ReposMode,
};
use super::overlay::{Confirm, Prompt};

pub const HELP: &[(&str, &str)] = &[
    ("↑↓ / jk", "move selection in the focused pane"),
    ("←→ / hl", "switch focus: repo list ↔ diagnostics"),
    ("Enter", "drill into the selected repository's packages"),
    ("/", "filter repositories"),
    ("Space", "toggle enabled/disabled"),
    ("a", "add a repository (URL or ppa:user/name)"),
    ("x", "remove a repository's source entry"),
    ("e", "edit the source file in $EDITOR"),
    ("m", "migrate a legacy .list entry to deb822 .sources"),
    ("d", "show removal impact (installed packages only here)"),
    ("f", "apply the selected diagnosis's fix"),
    ("A", "apply every auto-fixable diagnosis for this repo"),
    ("u", "run apt update (global; works from any tab)"),
    ("?", "toggle this help"),
    ("q", "quit"),
];

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    match app.repos.mode {
        ReposMode::Drill => super::packages::draw(frame, app, area),
        ReposMode::List => draw_list_mode(frame, app, area),
    }
}

fn draw_list_mode(frame: &mut Frame, app: &mut App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    draw_repo_list(frame, app, cols[0]);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(cols[1]);

    let repo_idx = selected_repo_idx(app);
    let diags: Vec<&Diagnosis> = match repo_idx {
        Some(idx) => app.diagnostics.for_repo(idx).collect(),
        None => Vec::new(),
    };
    let title = match repo_idx {
        Some(idx) => format!(
            "Diagnostics \u{2014} {}",
            app.data.repo(idx).map(|r| r.title()).unwrap_or_default()
        ),
        None => "Diagnostics".to_string(),
    };
    let selected = app
        .repos
        .diag_selected
        .min(diags.len().saturating_sub(1));
    let (diag_rows, diag_offset) =
        super::health::render_diag_list(frame, rows[0], &diags, selected, &title, app.theme);
    super::health::render_diag_detail(frame, rows[1], diags.get(selected).copied(), app.theme);
    app.layout.diag_rows = diag_rows;
    app.layout.diag_row_offset = diag_offset;
}

fn draw_repo_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let chunks = if app.repos.list.filter_active || !app.repos.list.filter_text.is_empty() {
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
    if chunks.len() == 2 {
        let filter_line = Paragraph::new(format!("/{}", app.repos.list.filter_text));
        frame.render_widget(filter_line, chunks[0]);
        idx = 1;
    }

    let items: Vec<ListItem> = app
        .repos
        .list
        .filtered
        .iter()
        .map(|&i| {
            let row = app.repos.list.rows[i];
            match row {
                RepoRow::NoRepo => {
                    let n = app.data.orphan_names.len();
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            "Not from any repository",
                            Style::default().add_modifier(Modifier::ITALIC),
                        ),
                        Span::raw(format!("  ({n})")),
                    ]))
                }
                RepoRow::Repo(r) => {
                    let repo = &app.data.repos[r];
                    let enabled_marker = if repo.enabled {
                        Span::styled("\u{25cf}", Style::default().fg(app.theme.ok))
                    } else {
                        Span::styled("\u{25cb}", Style::default().fg(app.theme.dim))
                    };
                    let mut spans = vec![enabled_marker, Span::raw(" "), Span::raw(repo.title())];
                    if app.data.mapping_available() && repo.enabled && !app.data.repo_has_files(r) {
                        spans.push(Span::styled(
                            "  [never fetched]",
                            Style::default().fg(app.theme.warn),
                        ));
                    }
                    if let Some(sev) = app.diagnostics.for_repo(r).map(|d| d.severity).max() {
                        let (icon, color) = match sev {
                            Severity::Error => ("\u{2717}", app.theme.error),
                            Severity::Warning => ("\u{26a0}", app.theme.warn),
                            Severity::Info => ("\u{2139}", app.theme.info),
                        };
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(icon, Style::default().fg(color)));
                    }
                    ListItem::new(Line::from(spans))
                }
            }
        })
        .collect();

    let mut state = ListState::default();
    if !app.repos.list.filtered.is_empty() {
        state.select(Some(app.repos.list.selected));
    }

    let border_color = if app.repos.focus == ReposFocus::Repos {
        app.theme.accent
    } else {
        app.theme.dim
    };
    let block = Block::default()
        .title(format!(
            "Repositories ({} configured)",
            app.data.repos.len()
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(chunks[idx]);
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, chunks[idx], &mut state);
    app.layout.repo_rows = inner;
    app.layout.repo_row_offset = state.offset();
}

pub fn handle_key<B: Backend + std::io::Write>(
    app: &mut App,
    key: KeyEvent,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    if app.repos.mode == ReposMode::Drill {
        if key.code == KeyCode::Esc {
            app.close_drill();
            return Ok(());
        }
        return super::packages::handle_key(app, key, terminal);
    }

    if app.repos.list.filter_active {
        match key.code {
            KeyCode::Esc => {
                app.repos.list.filter_active = false;
            }
            KeyCode::Enter => {
                app.repos.list.filter_active = false;
            }
            KeyCode::Backspace => {
                app.repos.list.filter_text.pop();
                app.refresh_repo_filter();
            }
            KeyCode::Char(c) => {
                app.repos.list.filter_text.push(c);
                app.refresh_repo_filter();
            }
            _ => {}
        }
        return Ok(());
    }

    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Left | KeyCode::Char('h') => app.repos.focus = ReposFocus::Repos,
        KeyCode::Right | KeyCode::Char('l') => app.repos.focus = ReposFocus::Diagnostics,
        KeyCode::Up | KeyCode::Char('k') => move_up(app),
        KeyCode::Down | KeyCode::Char('j') => move_down(app),
        KeyCode::Char('/') => {
            app.repos.list.filter_active = true;
        }
        KeyCode::Enter if app.repos.focus == ReposFocus::Repos => open_selected(app),
        KeyCode::Char(' ') if app.repos.focus == ReposFocus::Repos => toggle_selected(app),
        KeyCode::Char('a') => {
            app.prompt = Some((
                Prompt {
                    label: "Add repository (deb URI, or ppa:user/name)".into(),
                    input: String::new(),
                },
                PendingPrompt::AddRepo,
            ));
        }
        KeyCode::Char('x') => remove_selected(app),
        KeyCode::Char('e') => edit_selected(app, terminal)?,
        KeyCode::Char('m') => migrate_selected(app),
        KeyCode::Char('d') => show_removal_impact(app),
        KeyCode::Char('f') => queue_selected_fix(app),
        KeyCode::Char('A') => apply_all_safe_for_selected(app, terminal)?,
        _ => {}
    }
    Ok(())
}

fn move_up(app: &mut App) {
    match app.repos.focus {
        ReposFocus::Repos => {
            app.repos.list.selected = app.repos.list.selected.saturating_sub(1);
            app.repos.diag_selected = 0;
        }
        ReposFocus::Diagnostics => {
            app.repos.diag_selected = app.repos.diag_selected.saturating_sub(1);
        }
    }
}

fn move_down(app: &mut App) {
    match app.repos.focus {
        ReposFocus::Repos => {
            if app.repos.list.selected + 1 < app.repos.list.filtered.len() {
                app.repos.list.selected += 1;
            }
            app.repos.diag_selected = 0;
        }
        ReposFocus::Diagnostics => {
            let count = selected_repo_idx(app)
                .map(|i| app.diagnostics.for_repo(i).count())
                .unwrap_or(0);
            if app.repos.diag_selected + 1 < count {
                app.repos.diag_selected += 1;
            }
        }
    }
}

fn selected_row(app: &App) -> Option<RepoRow> {
    app.repos
        .list
        .filtered
        .get(app.repos.list.selected)
        .map(|&i| app.repos.list.rows[i])
}

pub(super) fn selected_repo_idx(app: &App) -> Option<usize> {
    match selected_row(app)? {
        RepoRow::Repo(idx) => Some(idx),
        _ => None,
    }
}

fn open_selected(app: &mut App) {
    match selected_row(app) {
        Some(RepoRow::NoRepo) => app.open_drill(PackageContext::NoRepo),
        Some(RepoRow::Repo(idx)) => app.open_drill(PackageContext::Repo(idx)),
        None => {}
    }
}

fn toggle_selected(app: &mut App) {
    let Some(idx) = selected_repo_idx(app) else {
        app.set_status("Select a repository to toggle (not \"Not from any repository\")", true);
        return;
    };
    let repo = &app.data.repos[idx];
    let verb = if repo.enabled { "Disable" } else { "Enable" };
    app.confirm = Some(PendingConfirm {
        ui: Confirm {
            title: "Toggle repository".into(),
            detail: vec![repo.title()],
            prompt: format!("{verb} this repository?"),
            alt: None,
        },
        action: PendingAction::ToggleRepoEnabled(idx),
        alt_action: None,
    });
}

/// Queue the global `u` (apt update) confirmation. Called from the global
/// key handler in `app.rs` — `u` works from any tab, not just this one.
pub fn queue_update(app: &mut App) {
    app.confirm = Some(PendingConfirm {
        ui: Confirm {
            title: "apt update".into(),
            detail: vec![
                "This refreshes every repository's package index and requires sudo.".into(),
            ],
            prompt: "Run `apt update` now?".into(),
            alt: None,
        },
        action: PendingAction::RunUpdate,
        alt_action: None,
    });
}

fn queue_selected_fix(app: &mut App) {
    let Some(idx) = selected_repo_idx(app) else {
        app.set_status("Select a repository first", true);
        return;
    };
    let Some(d) = app.diagnostics.for_repo(idx).nth(app.repos.diag_selected).cloned() else {
        app.set_status("No diagnosis selected", true);
        return;
    };
    super::health::queue_fix(app, &d);
}

fn apply_all_safe_for_selected<B: Backend + std::io::Write>(
    app: &mut App,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    let Some(idx) = selected_repo_idx(app) else {
        app.set_status("Select a repository first", true);
        return Ok(());
    };
    let diags: Vec<Diagnosis> = app.diagnostics.for_repo(idx).cloned().collect();
    super::health::apply_all_safe(app, &diags, terminal)
}

/// Actually flip the `enabled` state of every source entry backing a repo,
/// by rewriting the affected lines in place (comment/uncomment for one-line
/// entries, `Enabled: yes/no` for deb822).
pub fn toggle_enabled(app: &mut App, repo_idx: usize) -> anyhow::Result<String> {
    let repo = app.data.repos[repo_idx].clone();
    let target_enabled = !repo.enabled;
    for entry_id in &repo.entry_ids {
        let Some(entry) = app.data.sources.entry(*entry_id) else {
            continue;
        };
        let new_text = entry_text_with_enabled(entry, target_enabled);
        rewrite_span(app, entry.file.clone(), entry.span, &new_text)?;
    }
    Ok(format!(
        "{} {}",
        if target_enabled {
            "Enabled"
        } else {
            "Disabled"
        },
        repo.title()
    ))
}

/// Render an entry's text with its enabled state forced to `enabled`
/// (comment/uncomment for one-line, `Enabled: yes/no` for deb822).
fn entry_text_with_enabled(entry: &crate::apt::model::SourceEntry, enabled: bool) -> String {
    use crate::apt::model::SourceFormat;
    match entry.format {
        SourceFormat::OneLine => {
            let stripped = entry.raw.trim_start().trim_start_matches('#').trim_start();
            if enabled {
                stripped.to_string()
            } else {
                format!("# {stripped}")
            }
        }
        SourceFormat::Deb822 => {
            let mut lines: Vec<String> = entry
                .raw
                .lines()
                .filter(|l| !l.trim_start().to_ascii_lowercase().starts_with("enabled:"))
                .map(|l| l.to_string())
                .collect();
            if !enabled {
                lines.push("Enabled: no".to_string());
            }
            lines.join("\n")
        }
    }
}

/// Render an entry's text disabled — used by [`super::health`] for the
/// "disable" and "comment out duplicate" fixes, which are the same edit.
pub fn disabled_entry_text(entry: &crate::apt::model::SourceEntry) -> String {
    entry_text_with_enabled(entry, false)
}

/// Add (or replace) a `Signed-By`/`signed-by=` pointing at `key_path`,
/// scoping trust in that key to this one entry.
pub fn with_signed_by(
    entry: &crate::apt::model::SourceEntry,
    key_path: &std::path::Path,
) -> String {
    use crate::apt::model::SourceFormat;
    match entry.format {
        SourceFormat::Deb822 => {
            let mut lines: Vec<String> = entry
                .raw
                .lines()
                .filter(|l| {
                    !l.trim_start()
                        .to_ascii_lowercase()
                        .starts_with("signed-by:")
                })
                .map(|l| l.to_string())
                .collect();
            lines.push(format!("Signed-By: {}", key_path.display()));
            lines.join("\n")
        }
        SourceFormat::OneLine => {
            // Reconstructed from the parsed fields rather than via string
            // surgery on `entry.raw` — locating (and possibly replacing) an
            // existing `[...]` options block correctly is easy to get wrong,
            // whereas rebuilding the line from the already-parsed pieces
            // always produces a canonical, correct result.
            let opts = format!("signed-by={}", key_path.display());
            let arch = if entry.architectures.is_empty() {
                String::new()
            } else {
                format!("arch={} ", entry.architectures.join(","))
            };
            format!(
                "{} [{arch}{opts}] {} {} {}",
                entry.types.first().map(|s| s.as_str()).unwrap_or("deb"),
                entry.uris.join(" "),
                entry.suites.join(" "),
                entry.components.join(" "),
            )
        }
    }
}

/// Remove an entry's lines from its file entirely (used by the "remove
/// entry" fix and the repo-list `x` shortcut).
pub fn remove_span(
    app: &mut App,
    file: std::path::PathBuf,
    span: crate::apt::model::LineSpan,
) -> anyhow::Result<()> {
    let original = std::fs::read_to_string(&file)?;
    let lines: Vec<&str> = original.lines().collect();
    let mut out_lines: Vec<String> = lines[..span.start.min(lines.len())]
        .iter()
        .map(|s| s.to_string())
        .collect();
    out_lines.extend(
        lines[span.end.min(lines.len())..]
            .iter()
            .map(|s| s.to_string()),
    );
    let mut new_content = out_lines.join("\n");
    if !new_content.is_empty() {
        new_content.push('\n');
    }

    edit::backup(&file)?;
    let script = edit::privileged_write_script(&file, new_content.as_bytes());
    let status = crate::actions::privileges::sudo_command("sh", &["-c", &script]).status()?;
    if !status.success() {
        anyhow::bail!("writing {} as root failed", file.display());
    }
    app.reload_data()?;
    Ok(())
}

/// Replace the lines spanned by `span` in `file` with `new_text`, backing the
/// file up first. This is the one place that actually rewrites APT config.
pub fn rewrite_span(
    app: &mut App,
    file: std::path::PathBuf,
    span: crate::apt::model::LineSpan,
    new_text: &str,
) -> anyhow::Result<()> {
    let original = std::fs::read_to_string(&file)?;
    let lines: Vec<&str> = original.lines().collect();
    let mut out_lines: Vec<String> = lines[..span.start].iter().map(|s| s.to_string()).collect();
    out_lines.push(new_text.to_string());
    out_lines.extend(
        lines[span.end.min(lines.len())..]
            .iter()
            .map(|s| s.to_string()),
    );
    let mut new_content = out_lines.join("\n");
    if original.ends_with('\n') {
        new_content.push('\n');
    }

    edit::backup(&file)?;
    let script = edit::privileged_write_script(&file, new_content.as_bytes());
    let status = crate::actions::privileges::sudo_command("sh", &["-c", &script]).status()?;
    if !status.success() {
        anyhow::bail!("writing {} as root failed", file.display());
    }
    app.reload_data()?;
    Ok(())
}

fn remove_selected(app: &mut App) {
    let Some(idx) = selected_repo_idx(app) else {
        app.set_status("Select a repository to remove", true);
        return;
    };
    let repo = &app.data.repos[idx];
    let impacted = app.data.removal_impact(idx);
    let mut detail = vec![repo.title()];
    if !impacted.is_empty() {
        detail.push(format!(
            "{} installed package(s) are available only here and would be orphaned:",
            impacted.len()
        ));
        detail.extend(
            impacted
                .iter()
                .take(8)
                .map(|p| format!("  {} {}", p.name, p.version)),
        );
        if impacted.len() > 8 {
            detail.push(format!("  \u{2026} and {} more", impacted.len() - 8));
        }
    }
    app.confirm = Some(PendingConfirm {
        ui: Confirm {
            title: "Remove repository".into(),
            detail,
            prompt: "Remove this repository's source entry? (packages already installed are not removed)".into(),
            alt: None,
        },
        action: PendingAction::ApplyFix(Fix::RemoveEntry { entry_id: repo.entry_ids[0] }),
        alt_action: None,
    });
}

/// Edit a source file with the user's `$EDITOR`. Files under
/// `sources.list.d` are root-owned, so this goes through `sudo -e`
/// (`sudoedit`) rather than invoking `$EDITOR` directly: sudoedit copies the
/// file to a user-owned temp location, runs the editor unprivileged, and
/// copies the result back as root only if it changed — safer and simpler
/// than reimplementing that dance with our own privileged-write script.
fn edit_selected<B: Backend + std::io::Write>(
    app: &mut App,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    let Some(idx) = selected_repo_idx(app) else {
        app.set_status("Select a repository to edit", true);
        return Ok(());
    };
    let file = app.data.repos[idx]
        .entry_ids
        .first()
        .and_then(|id| app.data.sources.entry(*id))
        .map(|e| e.file.clone());
    let Some(file) = file else { return Ok(()) };

    edit::backup(&file)?;
    let status = app.with_suspended_terminal(terminal, |_| {
        Ok(std::process::Command::new("sudo")
            .arg("-e")
            .arg(&file)
            .status()?)
    })?;
    if status.success() {
        app.set_status(format!("Edited {}", file.display()), false);
        app.reload_data()?;
    } else {
        app.set_status("Edit cancelled or failed", true);
    }
    Ok(())
}

fn migrate_selected(app: &mut App) {
    let Some(idx) = selected_repo_idx(app) else {
        app.set_status("Select a repository to migrate", true);
        return;
    };
    let repo = &app.data.repos[idx];
    let Some(entry) = repo
        .entry_ids
        .first()
        .and_then(|id| app.data.sources.entry(*id))
    else {
        return;
    };
    if entry.format == crate::apt::model::SourceFormat::Deb822 {
        app.set_status("Already in deb822 (.sources) format", false);
        return;
    }
    let deb822_text = sources_to_deb822(entry);
    let entry_id = entry.id;
    app.confirm = Some(PendingConfirm {
        ui: Confirm {
            title: "Migrate to deb822".into(),
            detail: deb822_text.lines().map(|s| s.to_string()).collect(),
            prompt: "Rewrite this entry in the modern .sources format?".into(),
            alt: None,
        },
        action: PendingAction::MigrateEntry {
            entry_id,
            new_text: deb822_text,
        },
        alt_action: None,
    });
}

/// Write `new_text` as a sibling `.sources` file next to the entry's current
/// `.list` file, then remove the old file — both steps as root, backing up
/// the original first. As a correctness check, the new file should produce
/// the identical set of URIs under `apt-get update --print-uris` as the old
/// one (verified manually during development; not re-checked at runtime
/// since that would require a network round-trip on every migration).
pub fn apply_migration(
    app: &mut App,
    entry_id: usize,
    deb822_text: &str,
) -> anyhow::Result<String> {
    let entry = app
        .data
        .sources
        .entry(entry_id)
        .ok_or_else(|| anyhow::anyhow!("entry no longer exists"))?;
    let file = entry.file.clone();
    let new_path = file.with_extension("sources");
    edit::backup(&file)?;
    let script = format!(
        "{} && rm -f {}",
        edit::privileged_write_script(&new_path, deb822_text.as_bytes()),
        shell_escape(&file.to_string_lossy())
    );
    let status = crate::actions::privileges::sudo_command("sh", &["-c", &script]).status()?;
    if !status.success() {
        anyhow::bail!("migration write failed");
    }
    app.reload_data()?;
    Ok(format!("Migrated to {}", new_path.display()))
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Render a one-line source entry as a deb822 stanza.
fn sources_to_deb822(entry: &crate::apt::model::SourceEntry) -> String {
    let mut out = String::new();
    out.push_str(&format!("Types: {}\n", entry.types.join(" ")));
    out.push_str(&format!("URIs: {}\n", entry.uris.join(" ")));
    out.push_str(&format!("Suites: {}\n", entry.suites.join(" ")));
    if !entry.components.is_empty() {
        out.push_str(&format!("Components: {}\n", entry.components.join(" ")));
    }
    if !entry.architectures.is_empty() {
        out.push_str(&format!(
            "Architectures: {}\n",
            entry.architectures.join(" ")
        ));
    }
    if let Some(crate::apt::model::SignedBy::Path(p)) = &entry.signed_by {
        out.push_str(&format!("Signed-By: {}\n", p.display()));
    }
    if !entry.enabled {
        out.push_str("Enabled: no\n");
    }
    out
}

fn show_removal_impact(app: &mut App) {
    let Some(idx) = selected_repo_idx(app) else {
        app.set_status("Select a repository first", true);
        return;
    };
    let impacted = app.data.removal_impact(idx);
    if impacted.is_empty() {
        app.set_status(
            "No installed packages depend solely on this repository",
            false,
        );
        return;
    }
    let mut msg = format!("{} package(s) only available here: ", impacted.len());
    msg.push_str(
        &impacted
            .iter()
            .take(10)
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    );
    app.set_status(msg, false);
}

pub fn add_repo<B: Backend + std::io::Write>(
    app: &mut App,
    input: String,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(());
    }

    app.with_suspended_terminal(terminal, |_| Ok(()))?;
    if let Err(e) = app.privileges.ensure() {
        app.set_status(format!("Authentication failed: {e}"), true);
        return Ok(());
    }
    let (uri, suite, components) = if let Some(rest) = input.strip_prefix("ppa:") {
        let (owner, name) = rest.split_once('/').unwrap_or((rest, "ppa"));
        (
            format!("https://ppa.launchpadcontent.net/{owner}/{name}/ubuntu/"),
            app.host
                .codename
                .clone()
                .unwrap_or_else(|| "noble".to_string()),
            "main".to_string(),
        )
    } else {
        // Accept a bare deb URI; suite defaults to the host codename.
        (
            input.to_string(),
            app.host
                .codename
                .clone()
                .unwrap_or_else(|| "noble".to_string()),
            "main".to_string(),
        )
    };

    let line = format!("deb {uri} {suite} {components}\n");
    let path = new_source_path(&uri);
    let result = (|| -> anyhow::Result<()> {
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let mut content = existing;
        content.push_str(&line);
        let script = edit::privileged_write_script(&path, content.as_bytes());
        let status = crate::actions::privileges::sudo_command("sh", &["-c", &script]).status()?;
        if !status.success() {
            anyhow::bail!("writing {} as root failed", path.display());
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            app.set_status(format!("Added repository to {}", path.display()), false);
            let _ = app.reload_data();
        }
        Err(e) => app.set_status(format!("Could not add repository: {e}"), true),
    }
    Ok(())
}

/// Where to write a newly added repository: a fresh `.list` file under
/// `sources.list.d`, named after the URI's host so it's recognisable.
fn new_source_path(uri: &str) -> PathBuf {
    let host = crate::apt::model::strip_scheme(uri)
        .split('/')
        .next()
        .unwrap_or("custom")
        .replace(['.', ':'], "-");
    PathBuf::from(format!("/etc/apt/sources.list.d/{host}.list"))
}
