//! Audits GPG keyring files against what the configured sources actually
//! reference.
//!
//! Keys under `trusted.gpg.d` are trusted *globally* by APT for every
//! repository, regardless of `Signed-By` — so "no source references this
//! file" does not mean "unused", only "not scoped to any one repository".
//! The UI must say exactly that, and never offer to silently delete a key:
//! deletion always goes through a confirm + backup step.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::apt::model::SignedBy;
use crate::apt::sources::Sources;

/// Where a keyring file lives, which determines how it's trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyringScope {
    /// `/etc/apt/keyrings`, `/usr/share/keyrings`, or anywhere else pointed
    /// to by a `Signed-By` — trusted only for the repositories that
    /// reference it.
    PerRepo,
    /// `/etc/apt/trusted.gpg` or `/etc/apt/trusted.gpg.d/*` — trusted
    /// globally, independent of any `Signed-By`.
    Global,
}

#[derive(Debug, Clone)]
pub struct KeyringEntry {
    pub path: PathBuf,
    pub scope: KeyringScope,
    /// True when at least one source entry's `Signed-By` points at this file.
    pub referenced: bool,
    /// User IDs read from the key itself (`gpg --list-packets`), when available.
    pub uids: Vec<String>,
}

/// The directories APT actually reads keys from, rooted under `root`
/// (normally `/`, so this is `/etc/apt/keyrings` etc.; tests pass a fixture
/// root so the audit never touches the real system's keyrings).
pub fn keyring_dirs(root: &Path) -> Vec<PathBuf> {
    vec![
        root.join("etc/apt/keyrings"),
        root.join("usr/share/keyrings"),
        root.join("etc/apt/trusted.gpg.d"),
    ]
}

/// Build the full keyring inventory, cross-referenced against `sources`.
/// `root` scopes which directories are scanned (see [`keyring_dirs`]).
pub fn audit(sources: &Sources, root: &Path) -> Vec<KeyringEntry> {
    let referenced: std::collections::HashSet<PathBuf> = sources
        .entries
        .iter()
        .filter_map(|e| match &e.signed_by {
            Some(SignedBy::Path(p)) => fs::canonicalize(p).ok().or_else(|| Some(p.clone())),
            _ => None,
        })
        .collect();

    let dirs = keyring_dirs(root);

    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let scope = if dir.ends_with("trusted.gpg.d") {
            KeyringScope::Global
        } else {
            KeyringScope::PerRepo
        };
        for e in entries.flatten() {
            let path = e.path();
            if !path.is_file() {
                continue;
            }
            let canon = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            out.push(KeyringEntry {
                referenced: referenced.contains(&canon) || referenced.contains(&path),
                uids: read_uids(&path),
                path,
                scope,
            });
        }
    }

    // The legacy global keyring is a single file, not a directory of files.
    let legacy = root.join("etc/apt/trusted.gpg");
    if let Ok(meta) = fs::metadata(&legacy) {
        if meta.len() > 0 {
            out.push(KeyringEntry {
                uids: read_uids(&legacy),
                referenced: false,
                path: legacy,
                scope: KeyringScope::Global,
            });
        }
    }

    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Read user IDs out of a keyring file via `gpg --list-packets`, best-effort.
/// This is informational only (helps a human recognise "oh, that's the
/// Brave browser key") and never fails the audit if `gpg` is missing.
fn read_uids(path: &Path) -> Vec<String> {
    let output = Command::new("gpg")
        .args(["--list-packets", "--no-default-keyring"])
        .arg(path)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            l.strip_prefix("uid").map(|rest| rest.trim().to_string())
        })
        .collect()
}

/// Entries that are per-repo-scoped and unreferenced by any source: these are
/// the closest thing to a *safe* cleanup candidate, since (unlike global
/// keys) an unreferenced per-repo key genuinely cannot be used by APT for
/// anything.
pub fn orphaned_per_repo(entries: &[KeyringEntry]) -> Vec<&KeyringEntry> {
    entries
        .iter()
        .filter(|e| e.scope == KeyringScope::PerRepo && !e.referenced)
        .collect()
}

/// Turn an audit into health diagnoses: one per unreferenced keyring,
/// per-repo ones as a routine cleanup suggestion, global ones with an
/// explicit warning about the wider blast radius of removing them.
pub fn diagnose(sources: &Sources, root: &Path) -> Vec<super::Diagnosis> {
    use super::{DiagCode, Diagnosis, Scope, Severity};

    audit(sources, root)
        .into_iter()
        .filter(|e| !e.referenced)
        .map(|e| match e.scope {
            KeyringScope::PerRepo => {
                let uid_hint = e.uids.first().map(|u| format!(" ({u})")).unwrap_or_default();
                Diagnosis::new(
                    Severity::Info,
                    DiagCode::OrphanedKeyring { global: false },
                    format!(
                        "{}{uid_hint} is not referenced by any Signed-By and cannot be used by APT for anything",
                        e.path.display()
                    ),
                    Scope::Keyring(e.path),
                )
            }
            KeyringScope::Global => {
                let uid_hint = e.uids.first().map(|u| format!(" ({u})")).unwrap_or_default();
                Diagnosis::new(
                    Severity::Info,
                    DiagCode::OrphanedKeyring { global: true },
                    format!(
                        "{}{uid_hint} is trusted globally for every repository (no source scopes it); \
                         removing it revokes that trust everywhere, not just for one repo",
                        e.path.display()
                    ),
                    Scope::Keyring(e.path),
                )
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apt::model::{LineSpan, SourceEntry, SourceFormat};
    use std::io::Write;
    use tempfile::TempDir;

    fn make_sources(signed_by: PathBuf) -> Sources {
        let mut s = Sources::default();
        s.entries.push(SourceEntry {
            id: 0,
            file: PathBuf::from("/etc/apt/sources.list.d/x.list"),
            format: SourceFormat::OneLine,
            span: LineSpan::new(0, 1),
            enabled: true,
            types: vec!["deb".into()],
            uris: vec!["https://example.org".into()],
            suites: vec!["noble".into()],
            components: vec!["main".into()],
            architectures: vec![],
            signed_by: Some(SignedBy::Path(signed_by)),
            options: Default::default(),
            raw: String::new(),
        });
        s
    }

    #[test]
    fn referenced_keyring_is_marked_referenced() {
        let root = TempDir::new().unwrap();
        let keyring_dir = root.path().join("etc/apt/keyrings");
        fs::create_dir_all(&keyring_dir).unwrap();
        let key_path = keyring_dir.join("used.gpg");
        fs::File::create(&key_path)
            .unwrap()
            .write_all(b"fake key bytes")
            .unwrap();
        let unused_path = keyring_dir.join("unused.gpg");
        fs::File::create(&unused_path)
            .unwrap()
            .write_all(b"fake key bytes")
            .unwrap();

        let sources = make_sources(key_path.clone());
        let entries = audit(&sources, root.path());

        let used = entries.iter().find(|e| e.path == key_path).unwrap();
        assert!(used.referenced);
        let unused = entries.iter().find(|e| e.path == unused_path).unwrap();
        assert!(!unused.referenced);
    }

    #[test]
    fn orphaned_per_repo_excludes_referenced_and_global() {
        let root = TempDir::new().unwrap();
        let keyring_dir = root.path().join("etc/apt/keyrings");
        fs::create_dir_all(&keyring_dir).unwrap();
        let unused_path = keyring_dir.join("unused.gpg");
        fs::File::create(&unused_path)
            .unwrap()
            .write_all(b"x")
            .unwrap();

        let sources = make_sources(PathBuf::from("/nonexistent"));
        let entries = audit(&sources, root.path());
        let orphans = orphaned_per_repo(&entries);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].path, unused_path);
    }
}
