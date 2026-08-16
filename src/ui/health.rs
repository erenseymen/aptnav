//! Health diagnostics rendering and fix application — shared between the
//! Repos tab (one repository's diagnoses) and the Maintenance tab (global
//! diagnoses). Neither tab owns this file; both call into it.

use ratatui::backend::Backend;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use ratatui::Terminal;

use crate::health::fixes::{self, Fix};
use crate::health::{DiagCode, Diagnosis, Scope, Severity};

use super::app::{App, PendingAction, PendingConfirm};
use super::overlay::Confirm;
use super::theme::Theme;

/// Render a list of diagnoses into `area`. `selected` is clamped by the
/// caller — an out-of-range index here just means nothing is highlighted.
/// Returns the inner row area and the resulting scroll offset, for callers
/// that record them into `App.layout` for mouse hit-testing.
pub fn render_diag_list(
    frame: &mut Frame,
    area: Rect,
    diagnoses: &[&Diagnosis],
    selected: usize,
    title: &str,
    theme: Theme,
) -> (Rect, usize) {
    let items: Vec<ListItem> = diagnoses
        .iter()
        .map(|d| {
            let (icon, color) = match d.severity {
                Severity::Error => ("\u{2717}", theme.error),
                Severity::Warning => ("\u{26a0}", theme.warn),
                Severity::Info => ("\u{2139}", theme.info),
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
                Span::styled(fixable, Style::default().fg(theme.dim)),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    if !diagnoses.is_empty() {
        state.select(Some(selected));
    }
    let block = Block::default()
        .title(title.to_string())
        .borders(Borders::ALL);
    let inner = block.inner(area);
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, area, &mut state);
    (inner, state.offset())
}

pub fn render_diag_detail(frame: &mut Frame, area: Rect, diag: Option<&Diagnosis>, theme: Theme) {
    let lines = match diag {
        Some(d) => detail_lines(d, theme),
        None => vec![Line::from("No diagnoses")],
    };
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().title("Detail").borders(Borders::ALL)),
        area,
    );
}

fn detail_lines(d: &Diagnosis, theme: Theme) -> Vec<Line<'static>> {
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
            Span::styled("Suggested fix: ", Style::default().fg(theme.ok)),
            Span::raw(format!("{fix:?}")),
        ]));
    } else if let Some(explain) = fixes::explain_only(&d.code) {
        lines.push(Line::from(Span::styled(
            "Not fixable from a terminal:",
            Style::default().fg(theme.warn),
        )));
        lines.push(Line::from(explain));
    } else {
        lines.push(Line::from(Span::styled(
            "No automatic remedy; use the Repos tab's controls (disable/remove) if needed.",
            Style::default().fg(theme.dim),
        )));
    }
    lines
}

/// Queue a confirmation for the fix belonging to one diagnosis. Used by both
/// the Repos tab (`f` on a repo's diagnosis) and the Maintenance tab (`f` on
/// a global diagnosis).
pub fn queue_fix(app: &mut App, d: &Diagnosis) {
    let Some(fix) = resolve_fix(app, d) else {
        app.set_status("No automatic fix available for this diagnosis", true);
        return;
    };
    let desc = fix.describe(&app.data.sources);
    app.confirm = Some(PendingConfirm {
        ui: Confirm {
            title: "Apply fix".into(),
            detail: vec![d.message.clone()],
            prompt: format!("{desc}?"),
            alt: None,
        },
        action: PendingAction::ApplyFix(fix),
        alt_action: None,
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

/// Apply every bulk-safe fix in `diagnoses` without a per-item confirm,
/// reporting `Applied n/m` — the batching UX both the Repos tab's and the
/// Maintenance tab's `A` reuse verbatim. Reloads app data afterward, which
/// also rebuilds `app.diagnostics`; there is no separate "reopen" step
/// needed now that diagnostics live centrally.
pub fn apply_all_safe<B: Backend + std::io::Write>(
    app: &mut App,
    diagnoses: &[Diagnosis],
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    let fixable: Vec<Fix> = diagnoses
        .iter()
        .filter(|d| fixes::bulk_safe(d))
        .filter_map(|d| resolve_fix(app, d))
        .collect();
    if fixable.is_empty() {
        app.set_status("Nothing auto-fixable here", false);
        return Ok(());
    }
    let count = fixable.len();
    let descs: Vec<String> = fixable
        .iter()
        .map(|f| f.describe(&app.data.sources))
        .collect();

    app.with_suspended_terminal(terminal, |_| Ok(()))?;
    if let Err(e) = app.privileges.ensure() {
        app.set_status(format!("Authentication failed: {e}"), true);
        return Ok(());
    }

    let mut applied = 0;
    for fix in fixable {
        if apply_fix(app, &fix).is_ok() {
            applied += 1;
        }
    }
    app.set_status(
        format!("Applied {applied}/{count} fixes: {}", descs.join("; ")),
        applied < count,
    );
    let _ = app.reload_data();
    Ok(())
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
