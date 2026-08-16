//! Parses `/var/lib/dpkg/status` (what's installed) and
//! `/var/lib/apt/extended_states` (which installed packages were pulled in
//! automatically, i.e. as a dependency rather than by explicit request).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use super::model::InstallState;

/// One dpkg status stanza, reduced to the fields the UI needs.
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub architecture: String,
    pub state: InstallState,
    /// `hold` in the `Status:` line's want-field — orthogonal to `state`
    /// (`hold ok config-files` is a real, legal combination), so this is not
    /// folded into `InstallState`.
    pub held: bool,
    pub essential: bool,
    pub source: Option<String>, // `Source:` field, when it differs from the binary name
    pub fields: Vec<(String, String)>,
}

#[derive(Debug, Default)]
pub struct DpkgStatus {
    pub packages: HashMap<String, InstalledPackage>,
    /// Names marked `Auto-Installed: 1` in extended_states.
    pub auto_installed: HashMap<String, bool>,
}

impl DpkgStatus {
    pub fn is_installed(&self, name: &str) -> bool {
        self.packages
            .get(name)
            .is_some_and(|p| p.state.is_present())
    }

    pub fn is_auto(&self, name: &str) -> bool {
        self.auto_installed.get(name).copied().unwrap_or(false)
    }

    pub fn is_held(&self, name: &str) -> bool {
        self.packages.get(name).is_some_and(|p| p.held)
    }
}

/// Load dpkg's status database given the `Dir::State` root (normally
/// `/var/lib`, so this reads `<root>/dpkg/status` and
/// `<root>/apt/extended_states`).
pub fn load(state_root: &Path) -> Result<DpkgStatus> {
    let mut out = DpkgStatus::default();

    let status_path = state_root.join("dpkg/status");
    if status_path.is_file() {
        let text = fs::read_to_string(&status_path)
            .with_context(|| format!("reading {}", status_path.display()))?;
        for stanza in split_stanzas(&text) {
            if let Some(pkg) = parse_status_stanza(&stanza) {
                out.packages.insert(pkg.name.clone(), pkg);
            }
        }
    }

    let ext_path = state_root.join("apt/extended_states");
    if ext_path.is_file() {
        let text = fs::read_to_string(&ext_path)
            .with_context(|| format!("reading {}", ext_path.display()))?;
        for stanza in split_stanzas(&text) {
            let fields = parse_fields(&stanza);
            let name = fields
                .iter()
                .find(|(k, _)| k == "Package")
                .map(|(_, v)| v.clone());
            let auto = fields
                .iter()
                .find(|(k, _)| k == "Auto-Installed")
                .map(|(_, v)| v == "1")
                .unwrap_or(false);
            if let Some(name) = name {
                out.auto_installed.insert(name, auto);
            }
        }
    }

    Ok(out)
}

fn split_stanzas(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .collect()
}

fn parse_fields(stanza: &str) -> Vec<(String, String)> {
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in stanza.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = fields.last_mut() {
                last.1.push('\n');
                last.1.push_str(line);
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            fields.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    fields
}

fn parse_status_stanza(stanza: &str) -> Option<InstalledPackage> {
    let fields = parse_fields(stanza);
    let get = |k: &str| {
        fields
            .iter()
            .find(|(fk, _)| fk == k)
            .map(|(_, v)| v.as_str())
    };

    let name = get("Package")?.to_string();
    let version = get("Version").unwrap_or_default().to_string();
    let architecture = get("Architecture").unwrap_or_default().to_string();
    let source = get("Source").map(|s| {
        // `Source:` may carry a version in parens, e.g. `python3-defaults (3.12.3)`.
        s.split_whitespace().next().unwrap_or(s).to_string()
    });

    let (state, held) = match get("Status") {
        Some(status_line) => classify_status(status_line),
        None => (InstallState::NotInstalled, false),
    };
    let essential = get("Essential").is_some_and(|v| v == "yes");

    Some(InstalledPackage {
        name,
        version,
        architecture,
        state,
        held,
        essential,
        source,
        fields,
    })
}

/// `Status:` has three space-separated words: want, flag, status — e.g.
/// `install ok installed`, `hold ok installed`, `deinstall ok config-files`,
/// `install reinstreq half-installed`. `want` (`parts[0]`) is where `hold`
/// lives — it is dpkg's selection state, independent of what's actually
/// installed, so a held package can be in any install state.
fn classify_status(status_line: &str) -> (InstallState, bool) {
    let parts: Vec<&str> = status_line.split_whitespace().collect();
    let want = parts.first().copied().unwrap_or("");
    let flag = parts.get(1).copied().unwrap_or("");
    let status = parts.get(2).copied().unwrap_or("");
    let held = want == "hold";
    let state = match status {
        "installed" => {
            if flag == "ok" {
                InstallState::Installed
            } else {
                InstallState::Broken
            }
        }
        "config-files" => InstallState::ConfigFiles,
        "not-installed" => InstallState::NotInstalled,
        "half-installed" | "unpacked" | "half-configured" | "triggers-awaited"
        | "triggers-pending" => InstallState::Broken,
        _ => InstallState::NotInstalled,
    };
    (state, held)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write(dir: &TempDir, rel: &str, content: &str) {
        let p = dir.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn parses_installed_package() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "dpkg/status",
            "Package: 2to3\n\
             Status: install ok installed\n\
             Architecture: all\n\
             Source: python3-defaults\n\
             Version: 3.12.3-0ubuntu2.1\n\
             Description: 2to3 binary using python3\n\
             \x20More text.\n",
        );
        let db = load(dir.path()).unwrap();
        let pkg = db.packages.get("2to3").unwrap();
        assert_eq!(pkg.state, InstallState::Installed);
        assert_eq!(pkg.version, "3.12.3-0ubuntu2.1");
        assert_eq!(pkg.source.as_deref(), Some("python3-defaults"));
        assert!(db.is_installed("2to3"));
    }

    #[test]
    fn config_files_state_is_not_installed_but_present() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "dpkg/status",
            "Package: leftover\nStatus: deinstall ok config-files\nVersion: 1.0\n",
        );
        let db = load(dir.path()).unwrap();
        let pkg = db.packages.get("leftover").unwrap();
        assert_eq!(pkg.state, InstallState::ConfigFiles);
        assert!(!db.is_installed("leftover"));
    }

    #[test]
    fn source_field_with_version_in_parens_strips_version() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "dpkg/status",
            "Package: libfoo1\nStatus: install ok installed\nSource: foo (1.2.3)\nVersion: 1.2.3-1\n",
        );
        let db = load(dir.path()).unwrap();
        assert_eq!(
            db.packages.get("libfoo1").unwrap().source.as_deref(),
            Some("foo")
        );
    }

    #[test]
    fn extended_states_marks_auto_installed() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "apt/extended_states",
            "Package: intel-microcode\nArchitecture: amd64\nAuto-Installed: 1\n",
        );
        let db = load(dir.path()).unwrap();
        assert!(db.is_auto("intel-microcode"));
        assert!(!db.is_auto("something-else"));
    }

    #[test]
    fn multiple_stanzas_separated_by_blank_lines() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "dpkg/status",
            "Package: a\nStatus: install ok installed\nVersion: 1\n\n\
             Package: b\nStatus: install ok installed\nVersion: 2\n",
        );
        let db = load(dir.path()).unwrap();
        assert_eq!(db.packages.len(), 2);
        assert!(db.is_installed("a"));
        assert!(db.is_installed("b"));
    }

    #[test]
    fn classify_status_reads_the_want_field_for_hold() {
        assert_eq!(
            classify_status("hold ok installed"),
            (InstallState::Installed, true)
        );
        assert_eq!(
            classify_status("install ok installed"),
            (InstallState::Installed, false)
        );
        assert_eq!(
            classify_status("hold ok config-files"),
            (InstallState::ConfigFiles, true)
        );
        assert_eq!(
            classify_status("deinstall ok config-files"),
            (InstallState::ConfigFiles, false)
        );
        assert_eq!(
            classify_status("bogus"),
            (InstallState::NotInstalled, false)
        );
    }

    #[test]
    fn held_package_is_queryable_via_is_held() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "dpkg/status",
            "Package: vim\nStatus: hold ok installed\nVersion: 2:9.1-1\n\n\
             Package: git\nStatus: install ok installed\nVersion: 1:2.43-1\n",
        );
        let db = load(dir.path()).unwrap();
        assert!(db.is_held("vim"));
        assert!(!db.is_held("git"));
        assert!(!db.is_held("nonexistent"));
    }

    #[test]
    fn essential_field_is_parsed() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "dpkg/status",
            "Package: dpkg\nStatus: install ok installed\nEssential: yes\nVersion: 1\n\n\
             Package: vim\nStatus: install ok installed\nVersion: 1\n",
        );
        let db = load(dir.path()).unwrap();
        assert!(db.packages.get("dpkg").unwrap().essential);
        assert!(!db.packages.get("vim").unwrap().essential);
    }
}
