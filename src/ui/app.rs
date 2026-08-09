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
use crate::health::Diagnosis;

use super::filter::Filter;
use super::overlay::{Confirm, Prompt};
use super::{health as health_ui, packages as packages_ui, repos as repos_ui};

pub const LOG_CAPACITY: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Repos,
    Packages,
    Health,
}

/// What a row in the repository list represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoRow {
    AllPackages,
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
    pub labels: Vec<String>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub filter_text: String,
    pub filter_active: bool,
    pub scope: PackageScope,
}

pub struct HealthState {
    pub diagnoses: Vec<Diagnosis>,
    pub selected: usize,
    pub scope_repo: Option<usize>,
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
    PurgePackages(Vec<String>),
    RemovePackages(Vec<String>),
    ApplyFix(Fix),
    RunUpdate,
    MigrateEntry { entry_id: usize, new_text: String },
}

pub struct PendingConfirm {
    pub ui: Confirm,
    pub action: PendingAction,
}

pub enum PendingPrompt {
    AddRepo,
}

/// The `v` overlay on the package screen: every version of one package
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
    pub screen: Screen,
    pub repos: RepoListState,
    pub packages: Option<PackageListState>,
    pub health: Option<HealthState>,
    pub version_picker: Option<VersionPickerState>,
    pub filter: Filter,
    pub privileges: PrivilegeSession,
    pub log: VecDeque<String>,
    pub status: Option<(String, bool)>,
    pub confirm: Option<PendingConfirm>,
    pub prompt: Option<(Prompt, PendingPrompt)>,
    pub help_visible: bool,
    pub should_quit: bool,
}

impl App {
    pub fn new(data: AptData, roots: AptRoots) -> Self {
        let host = HostInfo::detect();
        let repos = build_repo_list(&data);
        let mut app = Self {
            data,
            roots,
            host,
            screen: Screen::Repos,
            repos,
            packages: None,
            health: None,
            version_picker: None,
            filter: Filter::new(),
            privileges: PrivilegeSession::new(),
            log: VecDeque::with_capacity(LOG_CAPACITY),
            status: None,
            confirm: None,
            prompt: None,
            help_visible: false,
            should_quit: false,
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
        self.repos.filtered = self
            .filter
            .apply(&self.repos.labels, &self.repos.filter_text);
        if self.repos.selected >= self.repos.filtered.len() {
            self.repos.selected = self.repos.filtered.len().saturating_sub(1);
        }
    }

    pub fn refresh_package_filter(&mut self) {
        if let Some(pkgs) = &mut self.packages {
            let scoped: Vec<usize> = pkgs
                .all_rows
                .iter()
                .enumerate()
                .filter(|(_, r)| match pkgs.scope {
                    PackageScope::All => true,
                    PackageScope::Installed => r.install_state.is_present(),
                    PackageScope::Upgradable => self.data.is_upgradable(&r.name),
                    PackageScope::NotInstalled => !r.install_state.is_present(),
                })
                .map(|(i, _)| i)
                .collect();
            let labels: Vec<String> = scoped
                .iter()
                .map(|&i| pkgs.all_rows[i].name.clone())
                .collect();
            let matched = self.filter.apply(&labels, &pkgs.filter_text);
            pkgs.filtered = matched.into_iter().map(|m| scoped[m]).collect();
            pkgs.labels = pkgs.all_rows.iter().map(|r| r.name.clone()).collect();
            if pkgs.selected >= pkgs.filtered.len() {
                pkgs.selected = pkgs.filtered.len().saturating_sub(1);
            }
        }
    }

    pub fn open_packages_for(&mut self, ctx: PackageContext) {
        let (rows, label) = match ctx {
            PackageContext::AllPackages => (self.data.all_packages(), "All packages".to_string()),
            PackageContext::NoRepo => (
                self.data.orphan_rows(),
                "Not from any repository".to_string(),
            ),
            PackageContext::Repo(idx) => {
                let title = self.data.repo(idx).map(|r| r.title()).unwrap_or_default();
                (self.data.packages_in_repo(idx), title)
            }
        };
        let labels: Vec<String> = rows.iter().map(|r| r.name.clone()).collect();
        let filtered: Vec<usize> = (0..rows.len()).collect();
        self.packages = Some(PackageListState {
            context: ctx,
            context_label: label,
            all_rows: rows,
            labels,
            filtered,
            selected: 0,
            filter_text: String::new(),
            filter_active: false,
            scope: PackageScope::All,
        });
        self.screen = Screen::Packages;
    }

    pub fn open_health_for(&mut self, repo_idx: Option<usize>) {
        let mut diagnoses = crate::health::checks::run(&self.data, &self.host);
        if let Some(idx) = repo_idx {
            diagnoses.retain(|d| matches!(d.scope, crate::health::Scope::Repo(r) if r == idx));
        } else {
            // The keyring audit has no single repository to scope to, so it
            // only shows up in the global health view (opened from "All
            // packages" / "Not from any repository", or via a plain `u`
            // update with no repo selected).
            diagnoses.extend(crate::health::keyrings::diagnose(
                &self.data.sources,
                std::path::Path::new("/"),
            ));
        }
        self.health = Some(HealthState {
            diagnoses,
            selected: 0,
            scope_repo: repo_idx,
        });
        self.screen = Screen::Health;
    }

    pub fn reload_data(&mut self) -> anyhow::Result<()> {
        let run_mapping = crate::apt::mapping::lists_dir_readable(&self.roots.lists_dir);
        self.data = AptData::load(self.roots.clone(), run_mapping)?;
        self.repos = build_repo_list(&self.data);
        self.refresh_repo_filter();
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
                if let Event::Key(key) = event::read()? {
                    if key.kind == crossterm::event::KeyEventKind::Press {
                        self.handle_key(key, terminal)?;
                    }
                }
            }
            if last_refresh_check.elapsed() > Duration::from_secs(60) {
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
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.execute_action(pending.action, terminal)?;
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

        match self.screen {
            Screen::Repos => repos_ui::handle_key(self, key, terminal)?,
            Screen::Packages => packages_ui::handle_key(self, key, terminal)?,
            Screen::Health => health_ui::handle_key(self, key, terminal)?,
        }
        Ok(())
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
                let diags = crate::health::update::classify(&logged.output);
                if let Some(h) = &mut self.health {
                    h.diagnoses = diags;
                } else {
                    self.health = Some(HealthState {
                        diagnoses: diags,
                        selected: 0,
                        scope_repo: None,
                    });
                    self.screen = Screen::Health;
                }
                let _ = self.reload_data();
            }
            PendingAction::ApplyFix(fix) => match health_ui::apply_fix(self, &fix) {
                Ok(msg) => {
                    self.set_status(msg, false);
                    let _ = self.reload_data();
                    self.open_health_for(self.health.as_ref().and_then(|h| h.scope_repo));
                }
                Err(e) => self.set_status(format!("{e}"), true),
            },
            PendingAction::MigrateEntry { entry_id, new_text } => {
                match repos_ui::apply_migration(self, entry_id, &new_text) {
                    Ok(msg) => self.set_status(msg, false),
                    Err(e) => self.set_status(format!("Migration failed: {e}"), true),
                }
            }
        }
        Ok(())
    }

    fn run_apt_action(&mut self, action: &str, pkgs: &[String]) -> anyhow::Result<()> {
        use crate::actions::apt as apt_actions;
        match apt_actions::run_action(action, pkgs) {
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
        if let Some(pkgs_state) = &mut self.packages {
            let ctx = pkgs_state.context;
            self.open_packages_for(ctx);
        }
        Ok(())
    }

    /// Record a privileged command's invocation and outcome in the log pane,
    /// so nothing that touches the system happens invisibly — the safety
    /// guarantee this app makes for every write.
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
        use crossterm::execute;
        use crossterm::terminal::{
            disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
        };
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        let result = f(self);
        enable_raw_mode()?;
        execute!(terminal.backend_mut(), EnterAlternateScreen)?;
        terminal.clear()?;
        result
    }
}

fn build_repo_list(data: &AptData) -> RepoListState {
    let mut rows = vec![RepoRow::AllPackages, RepoRow::NoRepo];
    rows.extend((0..data.repos.len()).map(RepoRow::Repo));
    let labels: Vec<String> = rows
        .iter()
        .map(|r| match r {
            RepoRow::AllPackages => "All packages".to_string(),
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
