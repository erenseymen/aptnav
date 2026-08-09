//! Shared modal widgets: confirmation dialogs, the help screen, a single-line
//! text prompt, and the status/log bar every screen renders at the bottom.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

/// A yes/no confirmation, optionally showing a multi-line detail (a dry-run
/// plan, a removal-impact list, …) above the prompt.
pub struct Confirm {
    pub title: String,
    pub detail: Vec<String>,
    pub prompt: String,
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

pub fn render_confirm(frame: &mut Frame, confirm: &Confirm) {
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
    lines.push(Line::from(vec![
        Span::styled(
            "y",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" confirm    "),
        Span::styled(
            "n/Esc",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" cancel"),
    ]));

    let block = Block::default()
        .title(confirm.title.as_str())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
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

pub fn render_help(frame: &mut Frame, lines: &[(&str, &str)]) {
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
                        .fg(Color::Cyan)
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
) {
    let (text, style) = match status {
        Some((msg, true)) => (msg.as_str(), Style::default().fg(Color::Red)),
        Some((msg, false)) => (msg.as_str(), Style::default().fg(Color::Green)),
        None => (hint, Style::default().fg(Color::DarkGray)),
    };
    let para = Paragraph::new(text).style(style).alignment(Alignment::Left);
    frame.render_widget(para, area);
}

/// The always-visible top title bar: app name, current screen, elevated/read-only marker.
pub fn render_title_bar(
    frame: &mut Frame,
    area: Rect,
    screen_name: &str,
    elevated: bool,
    load_ms: u128,
) {
    let marker = if elevated {
        Span::styled(
            " [elevated] ",
            Style::default().fg(Color::Black).bg(Color::Yellow),
        )
    } else {
        Span::styled(
            " [read-only] ",
            Style::default().fg(Color::Black).bg(Color::Cyan),
        )
    };
    let line = Line::from(vec![
        Span::styled(" aptnav ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("\u{2502} {screen_name} ")),
        marker,
        Span::styled(
            format!(" loaded in {load_ms}ms "),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

pub fn layout_screen(area: Rect) -> [Rect; 3] {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);
    [chunks[0], chunks[1], chunks[2]]
}
