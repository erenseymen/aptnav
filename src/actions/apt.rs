//! Runs the `apt-get` subcommands that change system state: `install`,
//! `purge`, `remove`, `upgrade`, `update`. Every mutating call has a
//! `dry_run` counterpart (`apt-get -s ...`, "simulate") so the UI can show
//! the full plan — packages installed/removed/upgraded/held back, disk
//! delta — before asking for confirmation.

use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result};

use super::privileges::sudo_command;

#[derive(Debug, Clone, Default)]
pub struct DryRunPlan {
    pub to_install: Vec<String>,
    pub to_upgrade: Vec<String>,
    pub to_remove: Vec<String>,
    pub held_back: Vec<String>,
    pub disk_delta: Option<String>,
    pub raw: String,
}

/// A logged record of a command this process ran, shown in the UI's
/// scrollable action log so nothing privileged happens silently.
#[derive(Debug, Clone)]
pub struct LoggedCommand {
    pub command: String,
    pub success: bool,
    pub output: String,
}

fn run_logged(mut cmd: Command) -> Result<LoggedCommand> {
    let command_str = format!("{:?}", cmd);
    let output: Output = cmd
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {command_str}"))?;
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(LoggedCommand {
        command: command_str,
        success: output.status.success(),
        output: combined,
    })
}

/// Build the argv `apt-get` receives, minus the `-s` simulate flag (added
/// separately by [`dry_run_with_opts`] since [`run_action_with_opts`]
/// doesn't want it). Pulled out as a pure function so the exact command
/// line — order of `-o` options relative to `-y` and the package list — is
/// testable without spawning a process.
fn build_args(action: &str, packages: &[String], opts: &[String]) -> Vec<String> {
    let mut args = vec![action.to_string(), "-y".to_string()];
    for o in opts {
        args.push("-o".to_string());
        args.push(o.clone());
    }
    args.extend(packages.iter().cloned());
    args
}

/// `apt-get -s install <pkgs>` (or purge/remove/upgrade) — simulated, makes
/// no changes, requires no privileges.
pub fn dry_run(action: &str, packages: &[String]) -> Result<DryRunPlan> {
    dry_run_with_opts(action, packages, &[])
}

/// As [`dry_run`], with extra `-o Key::Path=value` options — e.g. forcing a
/// phased update's dry-run to show what would happen if it weren't held
/// back.
pub fn dry_run_with_opts(action: &str, packages: &[String], opts: &[String]) -> Result<DryRunPlan> {
    let mut cmd = Command::new("apt-get");
    cmd.arg("-s");
    cmd.args(build_args(action, packages, opts));
    let logged = run_logged(cmd)?;
    Ok(parse_dry_run(&logged.output))
}

fn parse_dry_run(text: &str) -> DryRunPlan {
    let mut plan = DryRunPlan {
        raw: text.to_string(),
        ..Default::default()
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Inst ") {
            plan.to_install
                .push(rest.split_whitespace().next().unwrap_or(rest).to_string());
        } else if let Some(rest) = line.strip_prefix("Remv ") {
            plan.to_remove
                .push(rest.split_whitespace().next().unwrap_or(rest).to_string());
        } else if line.starts_with("After this operation") {
            plan.disk_delta = Some(line.trim().to_string());
        }
    }
    // `Inst` lines cover every install (upgrades included) but don't
    // distinguish "new" from "upgraded"; the summary blocks below do, so use
    // them to split `to_install` into the two categories the UI shows
    // separately.
    plan.held_back = extract_block(text, "The following packages have been kept back:");
    plan.to_upgrade = extract_block(text, "The following packages will be upgraded:");
    plan.to_install.retain(|p| !plan.to_upgrade.contains(p));
    plan
}

/// Pull the whitespace-separated package names out of one of apt-get's
/// indented summary blocks, e.g.:
/// ```text
/// The following packages will be upgraded:
///   foo bar baz
/// ```
fn extract_block(text: &str, header: &str) -> Vec<String> {
    let Some(start) = text.find(header) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for line in text[start + header.len()..].lines() {
        if line.is_empty() {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            break; // next section
        }
        names.extend(line.split_whitespace().map(|s| s.to_string()));
    }
    names
}

/// Run a real (non-simulated) `apt-get` action under sudo. Callers must have
/// already confirmed the [`DryRunPlan`] with the user and called
/// [`super::privileges::PrivilegeSession::ensure`].
pub fn run_action(action: &str, packages: &[String]) -> Result<LoggedCommand> {
    run_action_with_opts(action, packages, &[])
}

/// As [`run_action`], with extra `-o Key::Path=value` options — e.g.
/// `APT::Get::Always-Include-Phased-Updates=true` to force a phased upgrade
/// through instead of leaving it kept back.
pub fn run_action_with_opts(
    action: &str,
    packages: &[String],
    opts: &[String],
) -> Result<LoggedCommand> {
    let args = build_args(action, packages, opts);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_logged(sudo_command("apt-get", &arg_refs))
}

/// `apt-get update`, under sudo (refreshing `/var/lib/apt/lists` requires
/// root). Returns the full combined output for `health::update::classify`
/// to consume.
pub fn update() -> Result<LoggedCommand> {
    run_logged(sudo_command("apt-get", &["update"]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
Reading package lists...
Building dependency tree...
The following NEW packages will be installed:
  htop
The following packages will be upgraded:
  curl libcurl4
2 upgraded, 1 newly installed, 0 to remove and 3 not upgraded.
The following packages have been kept back:
  linux-generic
Inst htop (3.3.0-4build1 Ubuntu:24.04/noble-updates [amd64])
Inst curl [8.5.0-2ubuntu10] (8.5.0-2ubuntu10.6 Ubuntu:24.04/noble-updates [amd64])
Conf htop (3.3.0-4build1 Ubuntu:24.04/noble-updates [amd64])
After this operation, 1,234 kB of additional disk space will be used.
";

    #[test]
    fn parses_installs_upgrades_and_held_back() {
        let plan = parse_dry_run(SAMPLE);
        assert_eq!(plan.to_install, vec!["htop".to_string()]);
        assert_eq!(
            plan.to_upgrade,
            vec!["curl".to_string(), "libcurl4".to_string()]
        );
        assert_eq!(plan.held_back, vec!["linux-generic".to_string()]);
        assert_eq!(
            plan.disk_delta.as_deref(),
            Some("After this operation, 1,234 kB of additional disk space will be used.")
        );
    }

    #[test]
    fn parses_removal() {
        let text = "Remv old-package [1.0-1]\n";
        let plan = parse_dry_run(text);
        assert_eq!(plan.to_remove, vec!["old-package".to_string()]);
    }

    #[test]
    fn empty_output_yields_empty_plan() {
        let plan = parse_dry_run("");
        assert!(plan.to_install.is_empty());
        assert!(plan.to_remove.is_empty());
    }

    #[test]
    fn build_args_places_options_between_y_and_packages() {
        let args = build_args(
            "install",
            &["foo".to_string(), "bar".to_string()],
            &["APT::Get::Always-Include-Phased-Updates=true".to_string()],
        );
        assert_eq!(
            args,
            vec![
                "install",
                "-y",
                "-o",
                "APT::Get::Always-Include-Phased-Updates=true",
                "foo",
                "bar",
            ]
        );
    }

    #[test]
    fn build_args_with_no_opts_matches_the_plain_form() {
        let args = build_args("remove", &["foo".to_string()], &[]);
        assert_eq!(args, vec!["remove", "-y", "foo"]);
    }

    #[test]
    fn autoremove_dry_run_only_populates_to_remove() {
        let text = "\
Reading package lists...
Building dependency tree...
The following packages will be REMOVED:
  old-dep another-dep
0 upgraded, 0 newly installed, 2 to remove and 0 not upgraded.
Remv old-dep [1.0-1]
Remv another-dep [2.0-1]
";
        let plan = parse_dry_run(text);
        assert_eq!(
            plan.to_remove,
            vec!["old-dep".to_string(), "another-dep".to_string()]
        );
        assert!(plan.to_install.is_empty());
        assert!(plan.to_upgrade.is_empty());
        assert!(plan.held_back.is_empty());
    }
}
