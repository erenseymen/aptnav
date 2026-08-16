//! The Maintenance tab: global (non-repo-scoped) health diagnostics, plus a
//! package-maintenance task list (autoremove, cache cleanup, residual
//! config purge). Tasks are recomputed by `App::new` and every
//! `reload_data` — see `compute_maint_tasks` in `app.rs`.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;
use ratatui::Terminal;

use crate::health::Diagnosis;

use super::app::{App, MaintFocus, MaintTask, PendingAction, PendingConfirm};
use super::overlay::Confirm;

pub const HELP: &[(&str, &str)] = &[
    ("↑↓ / jk", "move selection in the focused pane"),
    ("←→ / hl", "switch focus: diagnostics ↔ tasks"),
    ("f", "apply the selected diagnosis's fix"),
    ("A", "apply every auto-fixable diagnosis"),
    ("Enter", "run the selected task"),
    ("c", "run autoremove, if there's anything to remove"),
    ("C", "clean the package cache, if there's anything cached"),
    ("?", "toggle this help"),
    ("q", "quit"),
];

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    draw_diagnostics(frame, app, cols[0]);
    draw_tasks(frame, app, cols[1]);
}

fn draw_diagnostics(frame: &mut Frame, app: &mut App, area: Rect) {
    let diags: Vec<&Diagnosis> = app.diagnostics.global_diagnoses().collect();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    let title = if app.data.mapping_available() {
        format!("Diagnostics ({})", diags.len())
    } else {
        format!(
            "Diagnostics ({}) \u{2014} some may not be attributable to a repository",
            diags.len()
        )
    };
    let selected = app.maint.diag_selected.min(diags.len().saturating_sub(1));
    let (diag_rows, diag_offset) =
        super::health::render_diag_list(frame, rows[0], &diags, selected, &title, app.theme);
    super::health::render_diag_detail(frame, rows[1], diags.get(selected).copied(), app.theme);
    app.layout.diag_rows = diag_rows;
    app.layout.diag_row_offset = diag_offset;
}

fn draw_tasks(frame: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = if app.maint.tasks.is_empty() {
        vec![ListItem::new("No maintenance tasks — everything looks clean")]
    } else {
        app.maint
            .tasks
            .iter()
            .map(|t| ListItem::new(task_label(t)))
            .collect()
    };
    let mut state = ListState::default();
    if !app.maint.tasks.is_empty() {
        state.select(Some(app.maint.task_selected));
    }
    let border_color = if app.maint.focus == MaintFocus::Tasks {
        app.theme.accent
    } else {
        app.theme.dim
    };
    let block = Block::default()
        .title(format!("Tasks ({})", app.maint.tasks.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(ratatui::style::Modifier::REVERSED));
    frame.render_stateful_widget(list, area, &mut state);
    app.layout.task_rows = inner;
    app.layout.task_row_offset = state.offset();
}

fn task_label(t: &MaintTask) -> String {
    match t {
        MaintTask::Autoremove { names } => {
            format!("Autoremove {} package(s): {}", names.len(), names.join(", "))
        }
        MaintTask::CleanCache { bytes } => format!("Clean package cache ({})", format_bytes(*bytes)),
        MaintTask::AutocleanCache => {
            "Autoclean cache (only obsolete, no-longer-downloadable .deb files)".to_string()
        }
        MaintTask::PurgeResidualConfigs { names } => format!(
            "Purge {} residual config package(s): {}",
            names.len(),
            names.join(", ")
        ),
    }
}

fn format_bytes(b: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if b >= MB {
        format!("{:.1} MB", b as f64 / MB as f64)
    } else if b >= KB {
        format!("{:.1} KB", b as f64 / KB as f64)
    } else {
        format!("{b} B")
    }
}

pub fn handle_key<B: Backend + std::io::Write>(
    app: &mut App,
    key: KeyEvent,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Left | KeyCode::Char('h') => app.maint.focus = MaintFocus::Diagnostics,
        KeyCode::Right | KeyCode::Char('l') => app.maint.focus = MaintFocus::Tasks,
        KeyCode::Up | KeyCode::Char('k') => move_up(app),
        KeyCode::Down | KeyCode::Char('j') => move_down(app),
        KeyCode::Char('f') if app.maint.focus == MaintFocus::Diagnostics => {
            queue_selected_fix(app)
        }
        KeyCode::Char('A') if app.maint.focus == MaintFocus::Diagnostics => {
            apply_all_safe(app, terminal)?
        }
        KeyCode::Enter if app.maint.focus == MaintFocus::Tasks => queue_selected_task(app),
        KeyCode::Char('c') => queue_task_kind(app, |t| matches!(t, MaintTask::Autoremove { .. }), "autoremove"),
        KeyCode::Char('C') => queue_task_kind(app, |t| matches!(t, MaintTask::CleanCache { .. }), "cache cleanup"),
        _ => {}
    }
    Ok(())
}

fn move_up(app: &mut App) {
    match app.maint.focus {
        MaintFocus::Diagnostics => {
            app.maint.diag_selected = app.maint.diag_selected.saturating_sub(1);
        }
        MaintFocus::Tasks => {
            app.maint.task_selected = app.maint.task_selected.saturating_sub(1);
        }
    }
}

fn move_down(app: &mut App) {
    match app.maint.focus {
        MaintFocus::Diagnostics => {
            let count = app.diagnostics.global_diagnoses().count();
            if app.maint.diag_selected + 1 < count {
                app.maint.diag_selected += 1;
            }
        }
        MaintFocus::Tasks => {
            if app.maint.task_selected + 1 < app.maint.tasks.len() {
                app.maint.task_selected += 1;
            }
        }
    }
}

fn queue_selected_fix(app: &mut App) {
    let Some(d) = app
        .diagnostics
        .global_diagnoses()
        .nth(app.maint.diag_selected)
        .cloned()
    else {
        app.set_status("No diagnosis selected", true);
        return;
    };
    super::health::queue_fix(app, &d);
}

fn apply_all_safe<B: Backend + std::io::Write>(
    app: &mut App,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    let diags: Vec<Diagnosis> = app.diagnostics.global_diagnoses().cloned().collect();
    super::health::apply_all_safe(app, &diags, terminal)
}

fn queue_selected_task(app: &mut App) {
    let Some(task) = app.maint.tasks.get(app.maint.task_selected) else {
        app.set_status("No task selected", true);
        return;
    };
    // `task_to_confirm` returns owned data, so its borrow of `task` (and
    // transitively of `app`) ends here — the `app.confirm = ...` mutation
    // right after is a separate, non-overlapping borrow.
    let (confirm, action) = task_to_confirm(task);
    app.confirm = Some(PendingConfirm {
        ui: confirm,
        action,
        alt_action: None,
    });
}

/// `c`/`C`: jump straight to queueing the first task matching `pred`,
/// regardless of cursor position — a shortcut for the common case where
/// there's exactly one autoremove or one cache-cleanup task on offer.
fn queue_task_kind(app: &mut App, pred: impl Fn(&MaintTask) -> bool, what: &str) {
    let Some(task) = app.maint.tasks.iter().find(|t| pred(t)) else {
        app.set_status(format!("Nothing to {what}"), false);
        return;
    };
    let (confirm, action) = task_to_confirm(task);
    app.confirm = Some(PendingConfirm {
        ui: confirm,
        action,
        alt_action: None,
    });
}

fn task_to_confirm(task: &MaintTask) -> (Confirm, PendingAction) {
    match task {
        MaintTask::Autoremove { names } => (
            Confirm {
                title: "Autoremove".into(),
                detail: vec![format!("{} package(s): {}", names.len(), names.join(", "))],
                prompt: "Run `apt-get autoremove`?".into(),
                alt: None,
            },
            PendingAction::RunAutoremove,
        ),
        MaintTask::CleanCache { bytes } => (
            Confirm {
                title: "Clean cache".into(),
                detail: vec![format!("Frees {}", format_bytes(*bytes))],
                prompt: "Run `apt-get clean`? This deletes every cached .deb.".into(),
                alt: None,
            },
            PendingAction::RunClean,
        ),
        MaintTask::AutocleanCache => (
            Confirm {
                title: "Autoclean cache".into(),
                detail: vec![
                    "Removes only cached .deb files that are no longer downloadable.".into(),
                ],
                prompt: "Run `apt-get autoclean`?".into(),
                alt: None,
            },
            PendingAction::RunAutoclean,
        ),
        MaintTask::PurgeResidualConfigs { names } => (
            Confirm {
                title: "Purge residual configs".into(),
                detail: vec![format!("{} package(s): {}", names.len(), names.join(", "))],
                prompt: "Purge these removed packages' leftover config files?".into(),
                alt: None,
            },
            PendingAction::PurgePackages(names.clone()),
        ),
    }
}
