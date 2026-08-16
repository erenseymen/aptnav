//! Central application state and the terminal event loop.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::Terminal;

use crate::actions::privileges::PrivilegeSession;
use crate::apt::model::Repo;
use crate::apt::{AptData, AptRoots, PackageRow};
use crate::health::checks::HostInfo;
use crate::health::fixes::Fix;
use crate::health::Diagnostics;

use super::filter::Filter;
use super::layout::LayoutCache;
use super::overlay::{Confirm, Prompt};
use super::selection::Selection;
use super::sort::SortState;
use super::theme::Theme;
use super::{
    health as health_ui, maint as maint_ui, packages as packages_ui, repos as repos_ui,
};

pub const LOG_CAPACITY: usize = 200;

/// The three top-level tabs. `Tab::ALL` is the single source of truth for
/// the tab bar, the numeric jump keys and `Tab`/`Shift-Tab` cycling — adding
/// a future tab (e.g. Transactions, once persistence lands) is one line here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Packages,
    Repos,
    Maintenance,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Packages, Tab::Repos, Tab::Maintenance];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Packages => "Packages",
            Tab::Repos => "Repos",
            Tab::Maintenance => "Maintenance",
        }
    }

    fn index(self) -> usize {
        Tab::ALL
            .iter()
            .position(|t| *t == self)
            .expect("Tab::ALL contains every variant")
    }

    pub fn next(self) -> Self {
        Tab::ALL[(self.index() + 1) % Tab::ALL.len()]
    }

    pub fn prev(self) -> Self {
        let n = Tab::ALL.len();
        Tab::ALL[(self.index() + n - 1) % n]
    }
}

/// What a row in the repository list represents. There is no longer a
/// synthetic "All packages" row here — that view is the Packages tab now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoRow {
    NoRepo,
    Repo(usize),
}

pub struct RepoListState {
    pub rows: Vec<RepoRow>,
    pub labels: Vec<String>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub filter_text: String,
    pub filter_active: bool,
}

/// Whether the Repos tab is showing the repo list (+ its diagnostics pane)
/// or has drilled into one repo's packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReposMode {
    List,
    Drill,
}

/// In `List` mode, which of the two side-by-side panes `j`/`k` moves within.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReposFocus {
    Repos,
    Diagnostics,
}

pub struct ReposTab {
    pub list: RepoListState,
    pub mode: ReposMode,
    pub focus: ReposFocus,
    /// The drilled-into repository's package list. `Some` iff `mode ==
    /// Drill`; kept as an `Option` rather than a always-present value
    /// because there is nothing meaningful to show before a repo is chosen.
    pub drill: Option<PackageListState>,
    pub diag_selected: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageScope {
    All,
    Installed,
    Upgradable,
    NotInstalled,
}

impl PackageScope {
    pub fn label(self) -> &'static str {
        match self {
            PackageScope::All => "all",
            PackageScope::Installed => "installed",
            PackageScope::Upgradable => "upgradable",
            PackageScope::NotInstalled => "not installed",
        }
    }
    pub fn next(self) -> Self {
        match self {
            PackageScope::All => PackageScope::Installed,
            PackageScope::Installed => PackageScope::Upgradable,
            PackageScope::Upgradable => PackageScope::NotInstalled,
            PackageScope::NotInstalled => PackageScope::All,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PackageContext {
    AllPackages,
    NoRepo,
    Repo(usize),
}

pub struct PackageListState {
    pub context: PackageContext,
    pub context_label: String,
    pub all_rows: Vec<PackageRow>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub filter_text: String,
    pub filter_active: bool,
    pub scope: PackageScope,
}

/// Which pane `j`/`k` moves within on the Maintenance tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintFocus {
    Diagnostics,
    Tasks,
}

/// A maintenance action surfaced on the Maintenance tab's Tasks pane.
pub enum MaintTask {
    Autoremove { names: Vec<String> },
    CleanCache { bytes: u64 },
    AutocleanCache,
    PurgeResidualConfigs { names: Vec<String> },
}

pub struct MaintTab {
    pub focus: MaintFocus,
    pub diag_selected: usize,
    pub task_selected: usize,
    pub tasks: Vec<MaintTask>,
}

/// An action queued behind a confirmation prompt.
///
/// File deletion (clutter files, empty source files) goes through
/// [`Fix::DeleteFile`] via `ApplyFix` uniformly rather than having its own
/// variant here, since every such deletion in this app originates from a
/// health diagnosis. Likewise "upgrade" reuses `InstallPackages`, since
/// `apt-get install <pkg>` on an already-installed package *is* an upgrade.
pub enum PendingAction {
    ToggleRepoEnabled(usize),
    InstallPackages(Vec<String>),
    /// Install/upgrade with `-o APT::Get::Always-Include-Phased-Updates=true`
    /// — the "force" side of the phased-update three-way confirm (`y`).
    /// `InstallPackages` on the same names is the "skip" side (`s`): apt
    /// naturally keeps phased packages back on its own when not forced.
    InstallPackagesForced(Vec<String>),
    PurgePackages(Vec<String>),
    RemovePackages(Vec<String>),
    ApplyFix(Fix),
    RunUpdate,
    MigrateEntry { entry_id: usize, new_text: String },
    RunAutoremove,
    RunClean,
    RunAutoclean,
}

pub struct PendingConfirm {
    pub ui: Confirm,
    pub action: PendingAction,
    /// The action bound to `Confirm::alt`'s key, if the confirm offers a
    /// third choice beyond confirm/cancel (currently only the phased-update
    /// force/skip/cancel prompt).
    pub alt_action: Option<PendingAction>,
}

pub enum PendingPrompt {
    AddRepo,
}

/// The `v` overlay on the Packages tab: every version of one package
/// across every repository that provides it, with its pin priority, via
/// `apt-cache policy` (see [`crate::apt::policy`]).
pub struct VersionPickerState {
    pub package: String,
    pub policy: crate::apt::policy::PackagePolicy,
    pub selected: usize,
}

pub struct App {
    pub data: AptData,
    pub roots: AptRoots,
    pub host: HostInfo,
    pub tab: Tab,
    pub repos: ReposTab,
    /// The Packages tab's package list. Always `PackageContext::AllPackages`
    /// — it no longer doubles as a generic "whatever was opened" view; that
    /// role now belongs to `repos.drill`.
    pub packages: PackageListState,
    pub maint: MaintTab,
    /// Every diagnosis from the last full health pass, pre-split by
    /// [`crate::health::target_of`] into per-repo and global buckets — see
    /// [`Diagnostics`]. The Repos tab reads `for_repo`; the Maintenance tab
    /// reads `global_diagnoses`.
    pub diagnostics: Diagnostics,
    /// Packages selected for a bulk action. Shared across the Packages tab
    /// and the Repos-tab drill view rather than per-view: a global set with
    /// a loud tab-bar badge, on the theory that a silent intersection with
    /// "whatever's currently visible" is exactly the kind of surprise a
    /// tool that runs `apt-get` must not spring on the user.
    pub selection: Selection,
    /// The package table's active column sort, shared across the Packages
    /// tab and the Repos-tab drill view — same rationale as `selection`.
    pub sort: SortState,
    /// Screen geometry recorded by the renderer this frame; consulted by
    /// mouse handling. The renderer is the only writer.
    pub layout: LayoutCache,
    pub version_picker: Option<VersionPickerState>,
    pub filter: Filter,
    pub privileges: PrivilegeSession,
    pub log: VecDeque<String>,
    pub status: Option<(String, bool)>,
    pub confirm: Option<PendingConfirm>,
    pub prompt: Option<(Prompt, PendingPrompt)>,
    pub help_visible: bool,
    pub should_quit: bool,
    pub theme: Theme,
    /// Set by `APTNAV_THEME` or the `T` manual toggle. While true, a later
    /// terminal background query (Step 2b) must not override `theme`.
    pub theme_forced: bool,
}

impl App {
    pub fn new(data: AptData, roots: AptRoots) -> Self {
        let host = HostInfo::detect();
        let repos = ReposTab {
            list: build_repo_list(&data),
            mode: ReposMode::List,
            focus: ReposFocus::Repos,
            drill: None,
            diag_selected: 0,
        };
        let packages = build_package_list(&data, PackageContext::AllPackages);
        let diagnostics = Diagnostics::rebuild(&data, &host);
        let maint_tasks = compute_maint_tasks(&data);
        let (theme_mode, theme_forced) =
            super::theme::resolve_from_env().unwrap_or((super::theme::ThemeMode::Dark, false));
        let mut app = Self {
            data,
            roots,
            host,
            tab: Tab::Packages,
            repos,
            packages,
            maint: MaintTab {
                focus: MaintFocus::Diagnostics,
                diag_selected: 0,
                task_selected: 0,
                tasks: maint_tasks,
            },
            diagnostics,
            selection: Selection::default(),
            sort: SortState::default(),
            layout: LayoutCache::default(),
            version_picker: None,
            filter: Filter::new(),
            privileges: PrivilegeSession::new(),
            log: VecDeque::with_capacity(LOG_CAPACITY),
            status: None,
            confirm: None,
            prompt: None,
            help_visible: false,
            should_quit: false,
            theme: Theme::for_mode(theme_mode),
            theme_forced,
        };
        if let Some(w) = app.data.mapping_warning.clone() {
            app.set_status(w, true);
        }
        app.refresh_repo_filter();
        app
    }

    pub fn set_status(&mut self, msg: impl Into<String>, is_error: bool) {
        let msg = msg.into();
        if self.log.len() == LOG_CAPACITY {
            self.log.pop_front();
        }
        self.log.push_back(msg.clone());
        self.status = Some((msg, is_error));
    }

    pub fn refresh_repo_filter(&mut self) {
        self.repos.list.filtered = self
            .filter
            .apply(&self.repos.list.labels, &self.repos.list.filter_text);
        if self.repos.list.selected >= self.repos.list.filtered.len() {
            self.repos.list.selected = self.repos.list.filtered.len().saturating_sub(1);
        }
    }

    /// The `PackageListState` currently on screen: the Packages tab's list,
    /// unless the Repos tab has drilled into a repository, in which case
    /// it's that drill view. This is the one place that decision is made,
    /// so `packages_ui`'s key/draw code can stay oblivious to which tab it's
    /// actually being called from.
    pub fn active_packages(&self) -> &PackageListState {
        if self.tab == Tab::Repos && self.repos.mode == ReposMode::Drill {
            self.repos
                .drill
                .as_ref()
                .expect("ReposMode::Drill always carries a drill state")
        } else {
            &self.packages
        }
    }

    pub fn active_packages_mut(&mut self) -> &mut PackageListState {
        if self.tab == Tab::Repos && self.repos.mode == ReposMode::Drill {
            self.repos
                .drill
                .as_mut()
                .expect("ReposMode::Drill always carries a drill state")
        } else {
            &mut self.packages
        }
    }

    pub fn refresh_active_package_filter(&mut self) {
        let sort = self.sort;
        if self.tab == Tab::Repos && self.repos.mode == ReposMode::Drill {
            if let Some(pkgs) = &mut self.repos.drill {
                refresh_package_filter(pkgs, &self.data, &mut self.filter, sort);
            }
        } else {
            refresh_package_filter(&mut self.packages, &self.data, &mut self.filter, sort);
        }
    }

    /// Enter the Repos tab's Drill mode for one repository (or "not from any
    /// repository"). Replaces the old cross-screen navigation from the repo
    /// list to a separate Packages screen.
    pub fn open_drill(&mut self, ctx: PackageContext) {
        let mut pkgs = build_package_list(&self.data, ctx);
        refresh_package_filter(&mut pkgs, &self.data, &mut self.filter, self.sort);
        self.repos.drill = Some(pkgs);
        self.repos.mode = ReposMode::Drill;
        self.repos.diag_selected = 0;
    }

    pub fn close_drill(&mut self) {
        self.repos.mode = ReposMode::List;
        self.repos.drill = None;
    }

    /// Whether the view the user is currently looking at has an open text
    /// filter capturing keystrokes — gates the global keys (`u`, the digit
    /// tab-jumps) that would otherwise steal a character out of a search.
    fn current_filter_active(&self) -> bool {
        match self.tab {
            Tab::Packages => self.packages.filter_active,
            Tab::Repos => match self.repos.mode {
                ReposMode::List => self.repos.list.filter_active,
                ReposMode::Drill => self
                    .repos
                    .drill
                    .as_ref()
                    .is_some_and(|d| d.filter_active),
            },
            Tab::Maintenance => false,
        }
    }

    /// Reload every on-disk data source and resync every view onto it:
    /// the repo list, both package lists (Packages tab and, if open, the
    /// Repos-tab drill), and the health diagnostics. This is the one place
    /// that resync happens, called after every mutating action — so no
    /// caller needs its own "now go re-open what I just changed" dance.
    pub fn reload_data(&mut self) -> anyhow::Result<()> {
        let run_mapping = crate::apt::mapping::lists_dir_readable(&self.roots.lists_dir);
        self.data = AptData::load(self.roots.clone(), run_mapping)?;
        self.repos.list = build_repo_list(&self.data);
        self.refresh_repo_filter();

        resync_package_list(&mut self.packages, &self.data, &mut self.filter, self.sort);
        if let Some(drill) = self.repos.drill.as_mut() {
            resync_package_list(drill, &self.data, &mut self.filter, self.sort);
        }

        self.diagnostics = Diagnostics::rebuild(&self.data, &self.host);
        self.maint.tasks = compute_maint_tasks(&self.data);
        if self.maint.task_selected >= self.maint.tasks.len() {
            self.maint.task_selected = self.maint.tasks.len().saturating_sub(1);
        }
        Ok(())
    }

    // -- event loop -------------------------------------------------------

    pub fn run<B: Backend + std::io::Write>(
        &mut self,
        terminal: &mut Terminal<B>,
    ) -> anyhow::Result<()> {
        let mut last_refresh_check = Instant::now();
        while !self.should_quit {
            terminal.draw(|f| super::draw(f, self))?;

            if event::poll(Duration::from_millis(250))? {
                match event::read()? {
                    Event::Key(key) => {
                        if key.kind == crossterm::event::KeyEventKind::Press {
                            self.handle_key(key, terminal)?;
                        }
                    }
                    Event::Mouse(mouse) => self.handle_mouse(mouse),
                    // The next `terminal.draw` picks up the new size on its
                    // own; ratatui/crossterm don't need anything else here.
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }

            if last_refresh_check.elapsed() >= Duration::from_secs(60) {
                self.privileges.maybe_refresh();
                last_refresh_check = Instant::now();
            }
        }
        Ok(())
    }

    fn handle_key<B: Backend + std::io::Write>(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<B>,
    ) -> anyhow::Result<()> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return Ok(());
        }

        if self.help_visible {
            if matches!(
                key.code,
                KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q')
            ) {
                self.help_visible = false;
            }
            return Ok(());
        }

        if let Some((prompt, kind)) = &mut self.prompt {
            match key.code {
                KeyCode::Esc => self.prompt = None,
                KeyCode::Enter => {
                    let input = prompt.input.clone();
                    let kind_owned = match kind {
                        PendingPrompt::AddRepo => PendingPrompt::AddRepo,
                    };
                    self.prompt = None;
                    self.submit_prompt(kind_owned, input, terminal)?;
                }
                KeyCode::Backspace => {
                    prompt.input.pop();
                }
                KeyCode::Char(c) => prompt.input.push(c),
                _ => {}
            }
            return Ok(());
        }

        if let Some(pending) = self.confirm.take() {
            let alt_key = pending.ui.alt.as_ref().map(|(k, _)| *k);
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.execute_action(pending.action, terminal)?;
                }
                KeyCode::Char(c)
                    if pending.alt_action.is_some() && alt_key.is_some_and(|a| a == c) =>
                {
                    self.execute_action(pending.alt_action.unwrap(), terminal)?;
                }
                _ => {
                    self.set_status("Cancelled", false);
                }
            }
            return Ok(());
        }

        if self.version_picker.is_some() {
            packages_ui::handle_version_picker_key(self, key, terminal)?;
            return Ok(());
        }

        if key.code == KeyCode::Char('?') {
            self.help_visible = true;
            return Ok(());
        }

        if key.code == KeyCode::Char('T') {
            self.theme = self.theme.toggled();
            self.theme_forced = true;
            return Ok(());
        }

        // Tab/Shift-Tab never collide with typed filter text (crossterm
        // never reports them as `KeyCode::Char`), so these are safe to
        // intercept globally regardless of what's focused.
        if key.code == KeyCode::Tab {
            self.tab = self.tab.next();
            return Ok(());
        }
        if key.code == KeyCode::BackTab {
            self.tab = self.tab.prev();
            return Ok(());
        }

        // `u` and the digit jumps *do* collide with typed filter text (e.g.
        // searching for a package containing "1" or "u"), so they're only
        // global when nothing on the current view is capturing keystrokes.
        if !self.current_filter_active() {
            match key.code {
                KeyCode::Char('1') => {
                    self.tab = Tab::Packages;
                    return Ok(());
                }
                KeyCode::Char('2') => {
                    self.tab = Tab::Repos;
                    return Ok(());
                }
                KeyCode::Char('3') => {
                    self.tab = Tab::Maintenance;
                    return Ok(());
                }
                KeyCode::Char('u') => {
                    repos_ui::queue_update(self);
                    return Ok(());
                }
                _ => {}
            }
        }

        match self.tab {
            Tab::Packages => packages_ui::handle_key(self, key, terminal)?,
            Tab::Repos => repos_ui::handle_key(self, key, terminal)?,
            Tab::Maintenance => maint_ui::handle_key(self, key, terminal)?,
        }
        Ok(())
    }

    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};

        // Modals own the whole screen while open; nothing in `layout` was
        // recorded for what's under them, so a hit test would resolve
        // against whatever was drawn before the modal opened.
        if self.help_visible
            || self.prompt.is_some()
            || self.confirm.is_some()
            || self.version_picker.is_some()
        {
            return;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.handle_mouse_click(mouse.column, mouse.row)
            }
            MouseEventKind::ScrollUp => self.handle_mouse_scroll(mouse.column, mouse.row, -3),
            MouseEventKind::ScrollDown => self.handle_mouse_scroll(mouse.column, mouse.row, 3),
            _ => {}
        }
    }

    fn handle_mouse_click(&mut self, x: u16, y: u16) {
        use super::layout::Hit;
        let Some(hit) = super::layout::hit_test(&self.layout, x, y) else {
            return;
        };
        match hit {
            Hit::Tab(tab) => self.tab = tab,
            Hit::SortHeader(col) => {
                self.sort.cycle_column(col);
                self.refresh_active_package_filter();
            }
            Hit::PackageRow { index, on_checkbox } => {
                // Scoped so the mutable borrow of the active package list
                // ends before `self.selection` (a different field) needs
                // its own mutable borrow below.
                let name = {
                    let pkgs = self.active_packages_mut();
                    if index < pkgs.filtered.len() {
                        pkgs.selected = index;
                    }
                    pkgs.filtered
                        .get(index)
                        .map(|&i| pkgs.all_rows[i].name.clone())
                };
                if on_checkbox {
                    if let Some(name) = name {
                        self.selection.toggle(&name);
                    }
                }
            }
            Hit::RepoRow { index } => {
                if self.tab == Tab::Repos
                    && self.repos.mode == ReposMode::List
                    && index < self.repos.list.filtered.len()
                {
                    self.repos.focus = ReposFocus::Repos;
                    self.repos.list.selected = index;
                    self.repos.diag_selected = 0;
                }
            }
            Hit::DiagRow { index } => match self.tab {
                Tab::Repos => {
                    self.repos.focus = ReposFocus::Diagnostics;
                    self.repos.diag_selected = index;
                }
                Tab::Maintenance => {
                    self.maint.focus = MaintFocus::Diagnostics;
                    self.maint.diag_selected = index;
                }
                Tab::Packages => {}
            },
            Hit::TaskRow { index } => {
                if self.tab == Tab::Maintenance && index < self.maint.tasks.len() {
                    self.maint.focus = MaintFocus::Tasks;
                    self.maint.task_selected = index;
                }
            }
        }
    }

    fn handle_mouse_scroll(&mut self, x: u16, y: u16, delta: i32) {
        use super::layout::Hit;
        match super::layout::hit_test(&self.layout, x, y) {
            Some(Hit::PackageRow { .. }) => self.move_active_package_cursor(delta),
            Some(Hit::RepoRow { .. }) => self.move_repo_cursor(delta),
            Some(Hit::DiagRow { .. }) => self.move_diag_cursor(delta),
            Some(Hit::TaskRow { .. }) => self.move_task_cursor(delta),
            _ => {}
        }
    }

    fn move_active_package_cursor(&mut self, delta: i32) {
        let pkgs = self.active_packages_mut();
        let len = pkgs.filtered.len();
        if len == 0 {
            return;
        }
        pkgs.selected = (pkgs.selected as i32 + delta).clamp(0, len as i32 - 1) as usize;
    }

    fn move_repo_cursor(&mut self, delta: i32) {
        if !(self.tab == Tab::Repos && self.repos.mode == ReposMode::List) {
            return;
        }
        let len = self.repos.list.filtered.len();
        if len == 0 {
            return;
        }
        self.repos.list.selected =
            (self.repos.list.selected as i32 + delta).clamp(0, len as i32 - 1) as usize;
        self.repos.diag_selected = 0;
    }

    fn move_diag_cursor(&mut self, delta: i32) {
        match self.tab {
            Tab::Repos => {
                let Some(idx) = repos_ui::selected_repo_idx(self) else {
                    return;
                };
                let len = self.diagnostics.for_repo(idx).count();
                if len == 0 {
                    return;
                }
                self.repos.diag_selected =
                    (self.repos.diag_selected as i32 + delta).clamp(0, len as i32 - 1) as usize;
            }
            Tab::Maintenance => {
                let len = self.diagnostics.global_diagnoses().count();
                if len == 0 {
                    return;
                }
                self.maint.diag_selected =
                    (self.maint.diag_selected as i32 + delta).clamp(0, len as i32 - 1) as usize;
            }
            Tab::Packages => {}
        }
    }

    fn move_task_cursor(&mut self, delta: i32) {
        let len = self.maint.tasks.len();
        if len == 0 {
            return;
        }
        self.maint.task_selected =
            (self.maint.task_selected as i32 + delta).clamp(0, len as i32 - 1) as usize;
    }

    fn submit_prompt<B: Backend + std::io::Write>(
        &mut self,
        kind: PendingPrompt,
        input: String,
        terminal: &mut Terminal<B>,
    ) -> anyhow::Result<()> {
        match kind {
            PendingPrompt::AddRepo => repos_ui::add_repo(self, input, terminal)?,
        }
        Ok(())
    }

    pub fn execute_action<B: Backend + std::io::Write>(
        &mut self,
        action: PendingAction,
        terminal: &mut Terminal<B>,
    ) -> anyhow::Result<()> {
        use crate::actions::apt as apt_actions;

        // Every remaining action mutates system state; ensure we hold a live
        // sudo credential first, suspending the TUI so the password prompt
        // (if any) is visible.
        self.with_suspended_terminal(terminal, |_| Ok(()))?; // flush draw state
        if let Err(e) = self.privileges.ensure() {
            self.set_status(format!("Authentication failed: {e}"), true);
            return Ok(());
        }

        match action {
            PendingAction::ToggleRepoEnabled(idx) => match repos_ui::toggle_enabled(self, idx) {
                Ok(msg) => self.set_status(msg, false),
                Err(e) => self.set_status(format!("{e}"), true),
            },
            PendingAction::InstallPackages(pkgs) => self.run_apt_action("install", &pkgs)?,
            PendingAction::InstallPackagesForced(pkgs) => self.run_apt_action_with_opts(
                "install",
                &pkgs,
                &["APT::Get::Always-Include-Phased-Updates=true".to_string()],
            )?,
            PendingAction::PurgePackages(pkgs) => self.run_apt_action("purge", &pkgs)?,
            PendingAction::RemovePackages(pkgs) => self.run_apt_action("remove", &pkgs)?,
            PendingAction::RunUpdate => {
                let logged = apt_actions::update()?;
                self.log_command(&logged);
                self.set_status(
                    if logged.success {
                        "apt update completed"
                    } else {
                        "apt update finished with errors"
                    },
                    !logged.success,
                );
                let live_diags = crate::health::update::classify(&logged.output);
                let _ = self.reload_data();
                self.diagnostics.merge_update_output(live_diags, &self.data);
            }
            PendingAction::ApplyFix(fix) => match health_ui::apply_fix(self, &fix) {
                Ok(msg) => {
                    self.set_status(msg, false);
                    let _ = self.reload_data();
                }
                Err(e) => self.set_status(format!("{e}"), true),
            },
            PendingAction::MigrateEntry { entry_id, new_text } => {
                match repos_ui::apply_migration(self, entry_id, &new_text) {
                    Ok(msg) => self.set_status(msg, false),
                    Err(e) => self.set_status(format!("Migration failed: {e}"), true),
                }
            }
            PendingAction::RunAutoremove => self.run_apt_action("autoremove", &[])?,
            PendingAction::RunClean => self.run_apt_action("clean", &[])?,
            PendingAction::RunAutoclean => self.run_apt_action("autoclean", &[])?,
        }
        Ok(())
    }

    fn run_apt_action(&mut self, action: &str, pkgs: &[String]) -> anyhow::Result<()> {
        self.run_apt_action_with_opts(action, pkgs, &[])
    }

    fn run_apt_action_with_opts(
        &mut self,
        action: &str,
        pkgs: &[String],
        opts: &[String],
    ) -> anyhow::Result<()> {
        use crate::actions::apt as apt_actions;
        // The set just acted on no longer needs to stay selected — matches
        // the "selection is a one-shot batch of targets" model everywhere
        // else (queueing, the confirm dialog). A no-op when nothing was
        // selected (the common single-cursor-row case).
        self.selection.clear();
        match apt_actions::run_action_with_opts(action, pkgs, opts) {
            Ok(logged) => {
                let success = logged.success;
                self.log_command(&logged);
                self.set_status(
                    if success {
                        format!("{action} succeeded for {}", pkgs.join(", "))
                    } else {
                        format!("{action} failed (see log)")
                    },
                    !success,
                );
            }
            Err(e) => self.set_status(format!("{e}"), true),
        }
        let _ = self.reload_data();
        Ok(())
    }

    /// Record a privileged command's invocation and outcome. Not yet
    /// rendered anywhere in the UI (see `TODO.md`'s log-viewer entry) — kept
    /// as a plain ring buffer for now.
    fn log_command(&mut self, logged: &crate::actions::apt::LoggedCommand) {
        if self.log.len() >= LOG_CAPACITY.saturating_sub(1) {
            self.log.pop_front();
        }
        self.log.push_back(format!(
            "$ {} -> {}",
            logged.command,
            if logged.success { "ok" } else { "failed" }
        ));
        for line in logged.output.lines().take(5) {
            if self.log.len() >= LOG_CAPACITY {
                self.log.pop_front();
            }
            self.log.push_back(format!("  {line}"));
        }
    }

    /// Suspend the alternate screen so a subprocess (an editor, `sudo`'s
    /// password prompt) can use the terminal directly, then restore it.
    pub fn with_suspended_terminal<B, F, T>(
        &mut self,
        terminal: &mut Terminal<B>,
        f: F,
    ) -> anyhow::Result<T>
    where
        B: Backend + std::io::Write,
        F: FnOnce(&mut App) -> anyhow::Result<T>,
    {
        use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
        use crossterm::execute;
        use crossterm::terminal::{
            disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
        };
        // Mouse capture must come off before the alternate screen does, or a
        // suspended subprocess (`sudo -e`, `sudoedit`'s `$EDITOR`) receives
        // raw mouse escape sequences on its stdin instead of the terminal.
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen
        )?;
        let result = f(self);
        enable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            EnterAlternateScreen,
            EnableMouseCapture
        )?;
        terminal.clear()?;
        result
    }
}

fn build_repo_list(data: &AptData) -> RepoListState {
    let mut rows = vec![RepoRow::NoRepo];
    rows.extend((0..data.repos.len()).map(RepoRow::Repo));
    let labels: Vec<String> = rows
        .iter()
        .map(|r| match r {
            RepoRow::NoRepo => "Not from any repository".to_string(),
            RepoRow::Repo(i) => data.repo(*i).map(Repo::title).unwrap_or_default(),
        })
        .collect();
    let filtered: Vec<usize> = (0..rows.len()).collect();
    RepoListState {
        rows,
        labels,
        filtered,
        selected: 0,
        filter_text: String::new(),
        filter_active: false,
    }
}

fn build_package_list(data: &AptData, ctx: PackageContext) -> PackageListState {
    let (rows, label) = match ctx {
        PackageContext::AllPackages => (data.all_packages(), "All packages".to_string()),
        PackageContext::NoRepo => (
            data.orphan_rows(),
            "Not from any repository".to_string(),
        ),
        PackageContext::Repo(idx) => {
            let title = data.repo(idx).map(|r| r.title()).unwrap_or_default();
            (data.packages_in_repo(idx), title)
        }
    };
    let filtered: Vec<usize> = (0..rows.len()).collect();
    PackageListState {
        context: ctx,
        context_label: label,
        all_rows: rows,
        filtered,
        selected: 0,
        filter_text: String::new(),
        filter_active: false,
        scope: PackageScope::All,
    }
}

/// The Maintenance tab's task list: what `apt-get -s autoremove` would
/// remove, how much the package cache holds, and which installed-package
/// rows are residual config files (`dpkg` kept the config, the binary is
/// long gone). Recomputed by `App::new` and every `reload_data` — the same
/// points every other cross-cutting view (diagnostics, both package lists)
/// already resyncs from.
fn compute_maint_tasks(data: &AptData) -> Vec<MaintTask> {
    let mut tasks = Vec::new();

    if let Ok(plan) = crate::actions::apt::dry_run("autoremove", &[]) {
        if !plan.to_remove.is_empty() {
            tasks.push(MaintTask::Autoremove {
                names: plan.to_remove,
            });
        }
    }

    if let Ok(bytes) = cache_archive_bytes() {
        if bytes > 0 {
            tasks.push(MaintTask::CleanCache { bytes });
            tasks.push(MaintTask::AutocleanCache);
        }
    }

    let residual: Vec<String> = data
        .dpkg
        .packages
        .values()
        .filter(|p| p.state == crate::apt::model::InstallState::ConfigFiles)
        .map(|p| p.name.clone())
        .collect();
    if !residual.is_empty() {
        tasks.push(MaintTask::PurgeResidualConfigs { names: residual });
    }

    tasks
}

/// Total size of every cached `.deb` under `/var/cache/apt/archives` — what
/// `apt-get clean` would free. Non-recursive by design: apt itself never
/// nests packages under subdirectories there (`partial/` holds in-progress
/// downloads, not cached debs, and is deliberately not counted).
fn cache_archive_bytes() -> std::io::Result<u64> {
    let mut total = 0u64;
    let dir = std::fs::read_dir("/var/cache/apt/archives")?;
    for entry in dir {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

/// Recompute `filtered` from `all_rows`/`scope`/`filter_text`, then apply
/// `sort` on top if one is active. A free function (not a method) so its
/// inputs — the list being filtered, the data it's filtered against, the
/// shared fuzzy matcher — stay disjoint field borrows at every call site
/// instead of being hidden behind a `&mut self` method that would force
/// them apart.
fn refresh_package_filter(
    pkgs: &mut PackageListState,
    data: &AptData,
    filter: &mut Filter,
    sort: SortState,
) {
    let scoped: Vec<usize> = pkgs
        .all_rows
        .iter()
        .enumerate()
        .filter(|(_, r)| match pkgs.scope {
            PackageScope::All => true,
            PackageScope::Installed => r.install_state.is_present(),
            PackageScope::Upgradable => data.is_upgradable(&r.name),
            PackageScope::NotInstalled => !r.install_state.is_present(),
        })
        .map(|(i, _)| i)
        .collect();
    let labels: Vec<String> = scoped.iter().map(|&i| pkgs.all_rows[i].name.clone()).collect();
    let matched = filter.apply(&labels, &pkgs.filter_text);
    pkgs.filtered = matched.into_iter().map(|m| scoped[m]).collect();
    // An active sort overrides nucleo's match-rank order; with no sort,
    // matched order (or name order, when there's no query) stands as-is.
    if sort.active.is_some() {
        let all_rows = &pkgs.all_rows;
        pkgs.filtered
            .sort_by(|&i, &j| super::sort::compare_rows(&all_rows[i], &all_rows[j], sort));
    }
    if pkgs.selected >= pkgs.filtered.len() {
        pkgs.selected = pkgs.filtered.len().saturating_sub(1);
    }
}

/// Rebuild a package list against fresh data while preserving what the user
/// was doing with it: same context, same filter text/scope, and the same
/// selected package by name (falling back to the top of the list if it's
/// gone). Used by `reload_data` for both the Packages tab and, if open, the
/// Repos-tab drill — every mutation resyncs both rather than leaving
/// whichever one isn't currently visible stale.
fn resync_package_list(
    pkgs: &mut PackageListState,
    data: &AptData,
    filter: &mut Filter,
    sort: SortState,
) {
    let selected_name = pkgs
        .filtered
        .get(pkgs.selected)
        .map(|&i| pkgs.all_rows[i].name.clone());
    let ctx = pkgs.context;
    let filter_text = std::mem::take(&mut pkgs.filter_text);
    let scope = pkgs.scope;

    *pkgs = build_package_list(data, ctx);
    pkgs.filter_text = filter_text;
    pkgs.scope = scope;
    refresh_package_filter(pkgs, data, filter, sort);

    if let Some(name) = selected_name {
        if let Some(pos) = pkgs.filtered.iter().position(|&i| pkgs.all_rows[i].name == name) {
            pkgs.selected = pos;
        }
    }
}
