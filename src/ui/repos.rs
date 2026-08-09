//! The repository list screen: the app's entry point. Shows "All packages",
//! "Not from any repository", then every configured repository, with
//! keyboard shortcuts to manage them.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use ratatui::Terminal;

use crate::actions::edit;
use crate::health::fixes::Fix;

use super::app::{App, PackageContext, PendingAction, PendingConfirm, PendingPrompt, RepoRow};
use super::overlay::{Confirm, Prompt};

pub const HELP: &[(&str, &str)] = &[
    ("↑↓ / jk", "move selection"),
    ("Enter", "open packages for the selected repository"),
    ("/", "filter repositories"),
    ("Space", "toggle enabled/disabled"),
    ("a", "add a repository (URL or ppa:user/name)"),
    ("x", "remove a repository's source entry"),
    ("e", "edit the source file in $EDITOR"),
    ("u", "run apt update"),
    ("h", "show health diagnostics for the selected repository"),
    ("m", "migrate a legacy .list entry to deb822 .sources"),
    ("i", "show removal impact (installed packages only here)"),
    ("?", "toggle this help"),
    ("q", "quit"),
];

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = if app.repos.filter_active || !app.repos.filter_text.is_empty() {
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
        let filter_line = Paragraph::new(format!("/{}", app.repos.filter_text));
        frame.render_widget(filter_line, chunks[0]);
        idx = 1;
    }

    let items: Vec<ListItem> = app
        .repos
        .filtered
        .iter()
        .map(|&i| {
            let row = app.repos.rows[i];
            match row {
                RepoRow::AllPackages => ListItem::new(Span::styled(
                    "All packages",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
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
                        Span::styled("\u{25cf}", Style::default().fg(Color::Green))
                    } else {
                        Span::styled("\u{25cb}", Style::default().fg(Color::DarkGray))
                    };
                    let mut spans = vec![enabled_marker, Span::raw(" "), Span::raw(repo.title())];
                    if app.data.mapping_available() && repo.enabled && !app.data.repo_has_files(r) {
                        spans.push(Span::styled(
                            "  [never fetched]",
                            Style::default().fg(Color::Yellow),
                        ));
                    }
                    ListItem::new(Line::from(spans))
                }
            }
        })
        .collect();

    let mut state = ListState::default();
    if !app.repos.filtered.is_empty() {
        state.select(Some(app.repos.selected));
    }

    let block = Block::default()
        .title(format!(
            "Repositories ({} configured)",
            app.data.repos.len()
        ))
        .borders(Borders::ALL);
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, chunks[idx], &mut state);
}

pub fn handle_key<B: Backend + std::io::Write>(
    app: &mut App,
    key: KeyEvent,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    if app.repos.filter_active {
        match key.code {
            KeyCode::Esc => {
                app.repos.filter_active = false;
            }
            KeyCode::Enter => {
                app.repos.filter_active = false;
            }
            KeyCode::Backspace => {
                app.repos.filter_text.pop();
                app.refresh_repo_filter();
            }
            KeyCode::Char(c) => {
                app.repos.filter_text.push(c);
                app.refresh_repo_filter();
            }
            _ => {}
        }
        return Ok(());
    }

    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Up | KeyCode::Char('k') => {
            app.repos.selected = app.repos.selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.repos.selected + 1 < app.repos.filtered.len() {
                app.repos.selected += 1;
            }
        }
        KeyCode::Char('/') => {
            app.repos.filter_active = true;
        }
        KeyCode::Enter => open_selected(app),
        KeyCode::Char(' ') => toggle_selected(app),
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
        KeyCode::Char('u') => {
            app.confirm = Some(PendingConfirm {
                ui: Confirm {
                    title: "apt update".into(),
                    detail: vec![
                        "This refreshes every repository's package index and requires sudo.".into(),
                    ],
                    prompt: "Run `apt update` now?".into(),
                },
                action: PendingAction::RunUpdate,
            });
        }
        KeyCode::Char('h') => {
            let repo_idx = selected_repo_idx(app);
            app.open_health_for(repo_idx);
        }
        KeyCode::Char('m') => migrate_selected(app),
        KeyCode::Char('i') => show_removal_impact(app),
        _ => {}
    }
    Ok(())
}

fn selected_row(app: &App) -> Option<RepoRow> {
    app.repos
        .filtered
        .get(app.repos.selected)
        .map(|&i| app.repos.rows[i])
}

fn selected_repo_idx(app: &App) -> Option<usize> {
    match selected_row(app)? {
        RepoRow::Repo(idx) => Some(idx),
        _ => None,
    }
}

fn open_selected(app: &mut App) {
    match selected_row(app) {
        Some(RepoRow::AllPackages) => app.open_packages_for(PackageContext::AllPackages),
        Some(RepoRow::NoRepo) => app.open_packages_for(PackageContext::NoRepo),
        Some(RepoRow::Repo(idx)) => app.open_packages_for(PackageContext::Repo(idx)),
        None => {}
    }
}

fn toggle_selected(app: &mut App) {
    let Some(idx) = selected_repo_idx(app) else {
        app.set_status("Select a repository to toggle (not \"All packages\")", true);
        return;
    };
    let repo = &app.data.repos[idx];
    let verb = if repo.enabled { "Disable" } else { "Enable" };
    app.confirm = Some(PendingConfirm {
        ui: Confirm {
            title: "Toggle repository".into(),
            detail: vec![repo.title()],
            prompt: format!("{verb} this repository?"),
        },
        action: PendingAction::ToggleRepoEnabled(idx),
    });
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
        },
        action: PendingAction::ApplyFix(Fix::RemoveEntry { entry_id: repo.entry_ids[0] }),
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
        },
        action: PendingAction::MigrateEntry {
            entry_id,
            new_text: deb822_text,
        },
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
