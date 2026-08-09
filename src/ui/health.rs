//! The health screen: static + live diagnostics for one repository or the
//! whole configuration, with guided and (where possible) automatic fixes.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use ratatui::Terminal;

use crate::health::fixes::{self, Fix};
use crate::health::{DiagCode, Diagnosis, Scope, Severity};

use super::app::{App, PendingAction, PendingConfirm, Screen};
use super::overlay::Confirm;

pub const HELP: &[(&str, &str)] = &[
    ("↑↓ / jk", "move selection"),
    ("Enter", "show explanation for the selected diagnosis"),
    ("f", "apply the suggested fix"),
    ("A", "apply every auto-fixable diagnosis"),
    ("Esc", "back to repositories"),
];

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let Some(h) = &app.health else { return };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    let title = match h.scope_repo {
        Some(idx) => format!(
            "Health \u{2014} {}",
            app.data.repo(idx).map(|r| r.title()).unwrap_or_default()
        ),
        None => format!(
            "Health \u{2014} all repositories ({} diagnoses)",
            h.diagnoses.len()
        ),
    };

    let items: Vec<ListItem> = h
        .diagnoses
        .iter()
        .map(|d| {
            let (icon, color) = match d.severity {
                Severity::Error => ("\u{2717}", Color::Red),
                Severity::Warning => ("\u{26a0}", Color::Yellow),
                Severity::Info => ("\u{2139}", Color::Cyan),
            };
            let fixable = if fixes::plan_fix(d).is_some() {
                " [fixable]"
            } else if fixes::explain_only(&d.code).is_some() {
                " [see explanation]"
            } else {
                ""
            };
            ListItem::new(Line::from(vec![
                Span::styled(icon, Style::default().fg(color)),
                Span::raw(" "),
                Span::raw(d.message.clone()),
                Span::styled(fixable, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    if !h.diagnoses.is_empty() {
        state.select(Some(h.selected));
    }
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, chunks[0], &mut state);

    let detail = h
        .diagnoses
        .get(h.selected)
        .map(|d| detail_lines(app, d))
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(Block::default().title("Detail").borders(Borders::ALL)),
        chunks[1],
    );
}

fn detail_lines(_app: &App, d: &Diagnosis) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            format!("{:?}", d.severity),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(d.message.clone()),
        Line::from(""),
    ];
    if let Some(fix) = fixes::plan_fix(d) {
        lines.push(Line::from(vec![
            Span::styled("Suggested fix: ", Style::default().fg(Color::Green)),
            Span::raw(format!("{fix:?}")),
        ]));
    } else if let Some(explain) = fixes::explain_only(&d.code) {
        lines.push(Line::from(Span::styled(
            "Not fixable from a terminal:",
            Style::default().fg(Color::Yellow),
        )));
        lines.push(Line::from(explain));
    } else {
        lines.push(Line::from(Span::styled(
            "No automatic remedy; use the repository screen's controls (disable/remove) if needed.",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines
}

pub fn handle_key<B: Backend + std::io::Write>(
    app: &mut App,
    key: KeyEvent,
    _terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    match key.code {
        KeyCode::Esc => app.screen = Screen::Repos,
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(h) = &mut app.health {
                h.selected = h.selected.saturating_sub(1);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(h) = &mut app.health {
                if h.selected + 1 < h.diagnoses.len() {
                    h.selected += 1;
                }
            }
        }
        KeyCode::Char('f') => queue_fix(app),
        KeyCode::Char('A') => apply_all_safe(app),
        _ => {}
    }
    Ok(())
}

fn selected_diag(app: &App) -> Option<Diagnosis> {
    app.health
        .as_ref()?
        .diagnoses
        .get(app.health.as_ref()?.selected)
        .cloned()
}

fn queue_fix(app: &mut App) {
    let Some(d) = selected_diag(app) else { return };
    let Some(fix) = resolve_fix(app, &d) else {
        app.set_status("No automatic fix available for this diagnosis", true);
        return;
    };
    let desc = fix.describe(&app.data.sources);
    app.confirm = Some(PendingConfirm {
        ui: Confirm {
            title: "Apply fix".into(),
            detail: vec![d.message.clone()],
            prompt: format!("{desc}?"),
        },
        action: PendingAction::ApplyFix(fix),
    });
}

/// [`fixes::plan_fix`] can't resolve every diagnosis on its own (some need
/// the diagnosis's repo scope resolved to a concrete entry id, e.g. duplicate
/// entries and suite mismatches); this fills that in.
fn resolve_fix(app: &App, d: &Diagnosis) -> Option<Fix> {
    if let Some(fix) = fixes::plan_fix(d) {
        return Some(fix);
    }
    match (&d.code, &d.scope) {
        (DiagCode::DuplicateEntry, Scope::Repo(idx)) => {
            let repo = app.data.repo(*idx)?;
            repo.entry_ids
                .get(1)
                .map(|&id| Fix::CommentDuplicate { entry_id: id })
        }
        (DiagCode::SuiteMismatch { .. }, Scope::Repo(idx)) => {
            let repo = app.data.repo(*idx)?;
            repo.entry_ids
                .first()
                .map(|&id| Fix::DisableEntry { entry_id: id })
        }
        (DiagCode::NoPubkey { keyid }, Scope::Repo(idx)) => {
            let repo = app.data.repo(*idx)?;
            repo.entry_ids.first().map(|&id| Fix::ResolveNoPubkey {
                entry_id: id,
                keyid: keyid.clone(),
            })
        }
        _ => None,
    }
}

fn apply_all_safe(app: &mut App) {
    let Some(h) = &app.health else { return };
    let fixable: Vec<Fix> = h
        .diagnoses
        .iter()
        .filter(|d| fixes::bulk_safe(d))
        .filter_map(|d| resolve_fix(app, d))
        .collect();
    if fixable.is_empty() {
        app.set_status("Nothing auto-fixable here", false);
        return;
    }
    let count = fixable.len();
    let descs: Vec<String> = fixable
        .iter()
        .map(|f| f.describe(&app.data.sources))
        .collect();
    // Batch application is expressed as repeated single-fix confirms would
    // be tedious; instead this applies them immediately since each one
    // individually is exactly what a user could already approve one at a
    // time with `f`, and `A` is explicitly the "apply every safe fix" bulk
    // action advertised in the help screen.
    let mut applied = 0;
    for fix in fixable {
        if health_apply(app, &fix).is_ok() {
            applied += 1;
        }
    }
    app.set_status(
        format!("Applied {applied}/{count} fixes: {}", descs.join("; ")),
        applied < count,
    );
    let _ = app.reload_data();
    app.open_health_for(app.health.as_ref().and_then(|h| h.scope_repo));
}

fn health_apply(app: &mut App, fix: &Fix) -> anyhow::Result<()> {
    app.privileges.ensure()?;
    apply_fix(app, fix).map(|_| ())
}

/// Execute a [`Fix`] against the live system. Called both from the
/// confirm-driven single-fix path (`f`) and from `apply_all_safe` (`A`).
pub fn apply_fix(app: &mut App, fix: &Fix) -> anyhow::Result<String> {
    use crate::actions::edit;
    match fix {
        Fix::DisableEntry { entry_id } => {
            let entry = app
                .data
                .sources
                .entry(*entry_id)
                .ok_or_else(|| anyhow::anyhow!("entry gone"))?;
            let file = entry.file.clone();
            let span = entry.span;
            let new_text = super::repos::disabled_entry_text(entry);
            super::repos::rewrite_span(app, file, span, &new_text)?;
            Ok("Entry disabled".to_string())
        }
        Fix::RemoveEntry { entry_id } => {
            let entry = app
                .data
                .sources
                .entry(*entry_id)
                .ok_or_else(|| anyhow::anyhow!("entry gone"))?;
            let file = entry.file.clone();
            super::repos::remove_span(app, file, entry.span)?;
            Ok("Entry removed".to_string())
        }
        Fix::CommentDuplicate { entry_id } => {
            let entry = app
                .data
                .sources
                .entry(*entry_id)
                .ok_or_else(|| anyhow::anyhow!("entry gone"))?;
            let file = entry.file.clone();
            let span = entry.span;
            let new_text = super::repos::disabled_entry_text(entry);
            super::repos::rewrite_span(app, file, span, &new_text)?;
            Ok("Duplicate entry commented out".to_string())
        }
        Fix::DeleteFile { path } => {
            edit::backup(path)?;
            std::fs::remove_file(path)?;
            Ok(format!("Deleted {}", path.display()))
        }
        Fix::ResolveNoPubkey { entry_id, keyid } => resolve_no_pubkey(app, *entry_id, keyid),
    }
}

fn resolve_no_pubkey(app: &mut App, entry_id: usize, keyid: &str) -> anyhow::Result<String> {
    let entry = app
        .data
        .sources
        .entry(entry_id)
        .ok_or_else(|| anyhow::anyhow!("entry gone"))?;
    let uri = entry.uris.first().cloned().unwrap_or_default();

    let resolved = if uri.contains("ppa.launchpad") {
        let rest = uri
            .split("ubuntu/")
            .next()
            .and_then(|_| uri.split('/').nth(3).zip(uri.split('/').nth(4)));
        match rest {
            Some((owner, name)) => fixes::resolve_launchpad_ppa_key(owner, name)
                .or_else(|_| fixes::fetch_key_from_keyserver(keyid)),
            None => fixes::fetch_key_from_keyserver(keyid),
        }
    } else {
        fixes::fetch_key_from_keyserver(keyid)
    }?;

    let path = fixes::dearmor_to_keyring(
        &resolved,
        &entry
            .file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("key")
            .replace(['.', ' '], "-"),
    )?;

    // Scope the key to this entry via Signed-By rather than trusting it
    // globally.
    let file = entry.file.clone();
    let span = entry.span;
    let new_text = super::repos::with_signed_by(entry, &path);
    super::repos::rewrite_span(app, file, span, &new_text)?;

    Ok(format!(
        "Trusted key {} (uid: {}) scoped to this repository via {}",
        resolved.fingerprint,
        resolved.uid,
        path.display()
    ))
}
