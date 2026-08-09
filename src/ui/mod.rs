//! The terminal UI: three screens (repositories, packages, health) plus
//! shared modal overlays (filter, confirm, prompt, help).

pub mod app;
pub mod filter;
pub mod health;
pub mod overlay;
pub mod packages;
pub mod repos;

use ratatui::layout::Rect;
use ratatui::Frame;

use app::{App, Screen};

pub fn draw(frame: &mut Frame, app: &App) {
    let full = frame.area();
    let [title_area, body_area, status_area] = overlay::layout_screen(full);

    let screen_name = match app.screen {
        Screen::Repos => "Repositories",
        Screen::Packages => "Packages",
        Screen::Health => "Health",
    };
    overlay::render_title_bar(
        frame,
        title_area,
        screen_name,
        app.privileges.is_elevated(),
        app.data.load_time.as_millis(),
    );

    draw_body(frame, app, body_area);

    let hint = match app.screen {
        Screen::Repos => "? help  /  filter  Space  toggle  a add  x remove  e edit  u update  h health  q quit",
        Screen::Packages => "? help  /  filter  Tab scope  i install  r remove  p purge  U upgrade  M auto/manual  Esc back",
        Screen::Health => "? help  f fix  A fix all  Esc back",
    };
    overlay::render_status_bar(frame, status_area, app.status.as_ref(), hint);

    if app.version_picker.is_some() {
        packages::render_version_picker(frame, app);
    }
    if let Some(pending) = &app.confirm {
        overlay::render_confirm(frame, &pending.ui);
    }
    if let Some((prompt, _)) = &app.prompt {
        overlay::render_prompt(frame, prompt);
    }
    if app.help_visible {
        let help = match app.screen {
            Screen::Repos => repos::HELP,
            Screen::Packages => packages::HELP,
            Screen::Health => health::HELP,
        };
        overlay::render_help(frame, help);
    }
}

fn draw_body(frame: &mut Frame, app: &App, area: Rect) {
    match app.screen {
        Screen::Repos => repos::draw(frame, app, area),
        Screen::Packages => packages::draw(frame, app, area),
        Screen::Health => health::draw(frame, app, area),
    }
}
