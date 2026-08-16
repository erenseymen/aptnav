//! Shared modal widgets: confirmation dialogs, the help overlay, a
//! single-line text prompt, the tab bar, and the status bar every tab
//! renders at the bottom.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::app::Tab;
use super::theme::Theme;

/// A yes/no confirmation, optionally showing a multi-line detail (a dry-run
/// plan, a removal-impact list, …) above the prompt.
pub struct Confirm {
    pub title: String,
    pub detail: Vec<String>,
    pub prompt: String,
    /// A third choice beyond confirm/cancel, e.g. `('s', "skip phased")`.
    /// `None` for a plain yes/no confirm. When set, the key it names is
    /// wired to `PendingConfirm::alt_action` by the caller.
    pub alt: Option<(char, String)>,
}

/// A single-line text input, used for "add repository" and similar.
pub struct Prompt {
    pub label: String,
    pub input: String,
}

pub fn centered_rect(pct_x: u16, pct_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

pub fn render_confirm(frame: &mut Frame, confirm: &Confirm, theme: Theme) {
    let area = centered_rect(
        70,
        60.min(20 + confirm.detail.len() as u16 * 3),
        frame.area(),
    );
    frame.render_widget(Clear, area);

    let mut lines: Vec<Line> = Vec::new();
    for d in &confirm.detail {
        lines.push(Line::from(d.as_str()));
    }
    if !confirm.detail.is_empty() {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        &confirm.prompt,
        Style::default().add_modifier(Modifier::BOLD),
    )));
    let mut choice_spans = vec![
        Span::styled(
            "y",
            Style::default().fg(theme.ok).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" confirm    "),
    ];
    if let Some((key, label)) = &confirm.alt {
        choice_spans.push(Span::styled(
            key.to_string(),
            Style::default()
                .fg(theme.warn)
                .add_modifier(Modifier::BOLD),
        ));
        choice_spans.push(Span::raw(format!(" {label}    ")));
    }
    choice_spans.push(Span::styled(
        "n/Esc",
        Style::default()
            .fg(theme.error)
            .add_modifier(Modifier::BOLD),
    ));
    choice_spans.push(Span::raw(" cancel"));
    lines.push(Line::from(choice_spans));

    let block = Block::default()
        .title(confirm.title.as_str())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border));
    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

pub fn render_prompt(frame: &mut Frame, prompt: &Prompt) {
    let area = centered_rect(60, 15, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(prompt.label.as_str())
        .borders(Borders::ALL);
    let text = format!("{}\u{2588}", prompt.input); // trailing block cursor
    let para = Paragraph::new(text).block(block);
    frame.render_widget(para, area);
}

pub fn render_help(frame: &mut Frame, lines: &[(&str, &str)], theme: Theme) {
    let area = centered_rect(70, 80, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title("Help (? or Esc to close)")
        .borders(Borders::ALL);
    let text: Vec<Line> = lines
        .iter()
        .map(|(key, desc)| {
            Line::from(vec![
                Span::styled(
                    format!("{key:>10}  "),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(*desc),
            ])
        })
        .collect();
    let para = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

/// The bottom status/log line: an error/info message when present, else the
/// most recent log entry, else a generic keybinding hint.
pub fn render_status_bar(
    frame: &mut Frame,
    area: Rect,
    status: Option<&(String, bool)>,
    hint: &str,
    theme: Theme,
) {
    let (text, style) = match status {
        Some((msg, true)) => (msg.as_str(), Style::default().fg(theme.error)),
        Some((msg, false)) => (msg.as_str(), Style::default().fg(theme.ok)),
        None => (hint, Style::default().fg(theme.dim)),
    };
    let para = Paragraph::new(text).style(style).alignment(Alignment::Left);
    frame.render_widget(para, area);
}

/// The always-visible top title bar: app name, current tab, elevated/read-only marker.
pub fn render_title_bar(
    frame: &mut Frame,
    area: Rect,
    screen_name: &str,
    elevated: bool,
    load_ms: u128,
    theme: Theme,
) {
    let marker = if elevated {
        Span::styled(
            " [elevated] ",
            Style::default().fg(theme.elevated_fg).bg(theme.elevated_bg),
        )
    } else {
        Span::styled(
            " [read-only] ",
            Style::default().fg(theme.readonly_fg).bg(theme.readonly_bg),
        )
    };
    let line = Line::from(vec![
        Span::styled(" aptnav ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("\u{2502} {screen_name} ")),
        marker,
        Span::styled(
            format!(" loaded in {load_ms}ms "),
            Style::default().fg(theme.dim),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// The persistent tab bar, rendered just below the title bar. `Tab::ALL` is
/// the single source of truth for what appears here — adding a tab variant
/// (e.g. once Transactions is unblocked) needs no change in this function.
/// Renders the tab bar and returns each tab's clickable `Rect`, computed
/// from the exact same loop that lays out the spans — the single source of
/// truth for "where is tab N on screen", so a later mouse click test can
/// never drift out of sync with what was actually drawn.
pub fn render_tab_bar(
    frame: &mut Frame,
    area: Rect,
    active: Tab,
    badge: Option<&str>,
    theme: Theme,
) -> Vec<(Rect, Tab)> {
    let mut spans = Vec::new();
    let mut hits = Vec::new();
    let mut x = area.x;
    for (i, tab) in Tab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
            x = x.saturating_add(2);
        }
        let label = format!(" {} ", tab.title());
        let width = label.chars().count() as u16;
        hits.push((Rect::new(x, area.y, width, 1), *tab));
        x = x.saturating_add(width);

        if *tab == active {
            spans.push(Span::styled(
                label,
                Style::default()
                    .fg(theme.tab_active_fg)
                    .bg(theme.tab_active_bg)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(label, Style::default().fg(theme.tab_inactive)));
        }
    }
    if let Some(b) = badge {
        spans.push(Span::raw("   "));
        spans.push(Span::styled(
            b,
            Style::default()
                .fg(theme.badge_selected)
                .add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    hits
}

pub fn layout_screen(area: Rect) -> [Rect; 4] {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title bar
            Constraint::Length(1), // tab bar
            Constraint::Min(3),    // body
            Constraint::Length(1), // status bar
        ])
        .split(area);
    [chunks[0], chunks[1], chunks[2], chunks[3]]
}
