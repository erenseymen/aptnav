//! The terminal UI: a persistent tab bar (Packages / Repos / Maintenance)
//! plus shared modal overlays (filter, confirm, prompt, help).

pub mod app;
pub mod filter;
pub mod health;
pub mod layout;
pub mod maint;
pub mod overlay;
pub mod packages;
pub mod repos;
pub mod selection;
pub mod sort;
pub mod theme;

use ratatui::layout::Rect;
use ratatui::Frame;

use app::{App, Tab};

pub fn draw(frame: &mut Frame, app: &mut App) {
    // Reset every frame: only the tab actually drawn this frame repopulates
    // its own hit-rects below, so whichever tab *isn't* on screen must not
    // keep serving mouse hits from the last time it was.
    app.layout = layout::LayoutCache::default();

    let full = frame.area();
    let [title_area, tabs_area, body_area, status_area] = overlay::layout_screen(full);

    overlay::render_title_bar(
        frame,
        title_area,
        app.tab.title(),
        app.privileges.is_elevated(),
        app.data.load_time.as_millis(),
        app.theme,
    );
    let badge = if app.selection.is_empty() {
        None
    } else {
        Some(format!("\u{25cf} {} selected", app.selection.len()))
    };
    app.layout.tab_hits =
        overlay::render_tab_bar(frame, tabs_area, app.tab, badge.as_deref(), app.theme);

    draw_body(frame, app, body_area);

    let hint = match app.tab {
        Tab::Packages => "? help  Tab switch  /  filter  Space select  a select filtered  A deselect filtered  s scope  i install  r remove  p purge  U upgrade  q quit",
        Tab::Repos => match app.repos.mode {
            app::ReposMode::List => "? help  Tab switch  /  filter  ←→ focus  Enter open  Space toggle  a add  x remove  e edit  m migrate  d impact  f fix  A fix all  u update  q quit",
            app::ReposMode::Drill => "? help  Esc back  /  filter  Space select  a/A select/deselect filtered  i install  r remove  p purge  U upgrade",
        },
        Tab::Maintenance => "? help  Tab switch  ←→ focus  f fix  A fix all  Enter run task  c autoremove  C clean cache  q quit",
    };
    overlay::render_status_bar(frame, status_area, app.status.as_ref(), hint, app.theme);

    if app.version_picker.is_some() {
        packages::render_version_picker(frame, app);
    }
    if let Some(pending) = &app.confirm {
        overlay::render_confirm(frame, &pending.ui, app.theme);
    }
    if let Some((prompt, _)) = &app.prompt {
        overlay::render_prompt(frame, prompt);
    }
    if app.help_visible {
        let help = match app.tab {
            Tab::Packages => packages::HELP,
            Tab::Repos => repos::HELP,
            Tab::Maintenance => maint::HELP,
        };
        overlay::render_help(frame, help, app.theme);
    }
}

fn draw_body(frame: &mut Frame, app: &mut App, area: Rect) {
    match app.tab {
        Tab::Packages => packages::draw(frame, app, area),
        Tab::Repos => repos::draw(frame, app, area),
        Tab::Maintenance => maint::draw(frame, app, area),
    }
}
