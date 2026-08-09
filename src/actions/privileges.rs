//! Keeps a `sudo` credential warm for the session so privileged actions
//! (installing packages, editing `/etc/apt`) don't re-prompt for a password
//! on every keystroke-triggered action, while the app itself keeps running
//! as the invoking user for everything read-only.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Tracks whether the session currently holds a live `sudo` credential.
/// `sudo -v` caches credentials for (typically) 15 minutes; this refreshes
/// at half that interval so a burst of actions never hits a stale timestamp.
pub struct PrivilegeSession {
    elevated: Arc<AtomicBool>,
    last_refresh: std::sync::Mutex<Option<Instant>>,
}

const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

impl PrivilegeSession {
    pub fn new() -> Self {
        Self {
            elevated: Arc::new(AtomicBool::new(false)),
            last_refresh: std::sync::Mutex::new(None),
        }
    }

    pub fn is_elevated(&self) -> bool {
        self.elevated.load(Ordering::SeqCst)
    }

    /// Ensure a valid `sudo` credential exists, prompting interactively if
    /// needed. Must be called with the terminal in a state that can show a
    /// password prompt (i.e. with the TUI's alternate screen suspended) —
    /// callers in `ui/` are responsible for that handoff.
    pub fn ensure(&self) -> anyhow::Result<()> {
        let needs_refresh = {
            let last = self.last_refresh.lock().unwrap();
            match *last {
                Some(t) => t.elapsed() > REFRESH_INTERVAL,
                None => true,
            }
        };
        if !needs_refresh && self.is_elevated() {
            return Ok(());
        }
        let status = Command::new("sudo").arg("-v").status()?;
        if !status.success() {
            anyhow::bail!("sudo authentication failed or was cancelled");
        }
        self.elevated.store(true, Ordering::SeqCst);
        *self.last_refresh.lock().unwrap() = Some(Instant::now());
        Ok(())
    }

    /// Best-effort background refresh so a long-idle session doesn't let the
    /// credential lapse mid-action; call periodically from the event loop.
    pub fn maybe_refresh(&self) {
        if !self.is_elevated() {
            return;
        }
        let stale = {
            let last = self.last_refresh.lock().unwrap();
            matches!(*last, Some(t) if t.elapsed() > REFRESH_INTERVAL)
        };
        if stale {
            let _ = Command::new("sudo").arg("-v").arg("-n").status();
            *self.last_refresh.lock().unwrap() = Some(Instant::now());
        }
    }
}

impl Default for PrivilegeSession {
    fn default() -> Self {
        Self::new()
    }
}

/// Wrap a command to run under the cached `sudo` credential. Callers must
/// have called [`PrivilegeSession::ensure`] first (typically via the UI's
/// confirm-before-privileged-action flow), so this never itself prompts.
pub fn sudo_command(program: &str, args: &[&str]) -> Command {
    let mut cmd = Command::new("sudo");
    cmd.arg("-n").arg(program).args(args);
    cmd
}
