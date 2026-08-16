mod actions;
mod apt;
mod health;
mod ui;

use std::io::stdout;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use apt::{AptData, AptRoots};

const HELP_TEXT: &str = "\
aptnav — a repository-centric TUI for browsing and managing APT repositories and packages.

USAGE:
    aptnav [OPTIONS]

OPTIONS:
    --apt-root <DIR>   Read APT state from <DIR>/etc/apt, <DIR>/var/lib/apt/lists and
                        <DIR>/var/lib/dpkg instead of the real system. Useful for
                        exploring a fixture tree offline; writes are still attempted
                        against the real paths for privileged actions, so this is
                        primarily a read-only exploration mode.
    -h, --help         Print this help and exit.
";

fn main() -> Result<()> {
    let mut apt_root: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--apt-root" => {
                apt_root = args.next().map(PathBuf::from);
            }
            "-h" | "--help" => {
                print!("{HELP_TEXT}");
                return Ok(());
            }
            other => {
                eprintln!("aptnav: unrecognized argument '{other}'\n");
                print!("{HELP_TEXT}");
                std::process::exit(2);
            }
        }
    }

    let roots = match &apt_root {
        Some(root) => AptRoots::under(root),
        None => AptRoots::default(),
    };
    let run_indextargets = apt_root.is_none() && apt::mapping::lists_dir_readable(&roots.lists_dir);

    eprintln!("aptnav: loading APT state...");
    let data = AptData::load(roots.clone(), run_indextargets)?;
    eprintln!(
        "aptnav: {} packages indexed across {} repositories in {:.0?}",
        data.lists.stubs.len(),
        data.repos.len(),
        data.load_time
    );

    let mut app = ui::app::App::new(data, roots);

    enable_raw_mode()?;

    // Tiers 1-2 (env vars) were already resolved in `App::new`. This is Tier
    // 3: ask the terminal itself via OSC 11. It must run between
    // `enable_raw_mode` (so the reply arrives byte-by-byte, unbuffered, not
    // echoed) and `EnterAlternateScreen` (so the query/reply exchange
    // happens on the normal screen, not the one the app is about to draw
    // into). A forced theme (env var or, later, a manual toggle before this
    // point — there isn't one yet, but the check is here for that reason)
    // always wins over detection.
    if !app.theme_forced {
        if let Some(mode) = ui::theme::query_terminal(Duration::from_millis(100)) {
            app.theme = ui::theme::Theme::for_mode(mode);
        }
    }

    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Ensure the terminal is always restored, even on panic.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        default_hook(info);
    }));

    let result = app.run(&mut terminal);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    result
}
