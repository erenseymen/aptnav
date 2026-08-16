//! Repository health: static diagnostics, `apt-get update` failure
//! classification, keyring auditing, and auto-fix planning/execution.

pub mod checks;
pub mod fixes;
pub mod keyrings;
pub mod update;

use std::path::{Path, PathBuf};

use crate::apt::AptData;

/// How serious a diagnosis is, driving sort order and icon in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// What part of the configuration a diagnosis is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Repo(usize),
    Entry(usize),
    File(PathBuf),
    Keyring(PathBuf),
    Global,
}

/// The specific condition detected. Kept as data (not just a message string)
/// so [`fixes`] can match on it to decide what remedy, if any, applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagCode {
    EmptySourceFile,
    ClutterFile,
    NeverFetched,
    StaleList {
        days: i64,
    },
    SuiteMismatch {
        entry_suite: String,
        host_codename: String,
    },
    DuplicateEntry,
    LegacyFormat,
    SignedByMissing,
    DeprecatedTrustedGpg,
    InsecureHttp,
    UnauthenticatedTrusted,
    MissingHostArch,
    NoPubkey {
        keyid: String,
    },
    NotFound404 {
        url: String,
    },
    ReleaseNotValidYet,
    ReleaseExpired,
    CertVerificationFailed,
    ConflictingDistribution,
    ConnectionFailed,
    /// A keyring file no source entry's `Signed-By` points at. `global` marks
    /// files under `trusted.gpg.d` (or the legacy `trusted.gpg`), which are
    /// trusted for *every* repository regardless of `Signed-By` — "no owning
    /// source" there means something different from a per-repo keyring that
    /// truly cannot be used by APT for anything.
    OrphanedKeyring {
        global: bool,
    },
    Other {
        raw: String,
    },
}

#[derive(Debug, Clone)]
pub struct Diagnosis {
    pub severity: Severity,
    pub code: DiagCode,
    pub message: String,
    pub scope: Scope,
}

impl Diagnosis {
    pub fn new(
        severity: Severity,
        code: DiagCode,
        message: impl Into<String>,
        scope: Scope,
    ) -> Self {
        Self {
            severity,
            code,
            message: message.into(),
            scope,
        }
    }
}

/// Which tab a diagnosis belongs to: the Repos tab (one or more specific
/// repositories) or the Maintenance tab (everything else).
///
/// This is *not* a simple match on [`Scope`]'s variant — `update::classify`
/// (a failed `apt update`) reports `Scope::File` pointing at a `Packages`
/// index under `/var/lib/apt/lists`, which is exactly the kind of
/// per-repository failure (`NO_PUBKEY`, a 404, an expired `Release`, a TLS
/// failure) that must land on its repository in the Repos tab, not in
/// Maintenance. `Scope::Entry` is also not a 1:1 mapping: one source entry
/// expands into one repository per suite×component, so a single entry-level
/// diagnosis (e.g. a missing `Signed-By`) can legitimately belong to several
/// repositories at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Repos(Vec<usize>),
    Global,
}

/// Decide which repositories (if any) a diagnosis belongs to. The single
/// place this decision is made — see [`Target`] for why it isn't a plain
/// match on `Scope`.
pub fn target_of(d: &Diagnosis, data: &AptData) -> Target {
    match &d.scope {
        Scope::Repo(i) => Target::Repos(vec![*i]),
        Scope::Entry(id) => {
            let repos = repos_owning_entry(data, *id);
            if repos.is_empty() {
                Target::Global
            } else {
                Target::Repos(repos)
            }
        }
        Scope::File(path) => target_of_file(path, data),
        Scope::Keyring(_) => Target::Global,
        Scope::Global => Target::Global,
    }
}

fn repos_owning_entry(data: &AptData, entry_id: usize) -> Vec<usize> {
    data.repos
        .iter()
        .enumerate()
        .filter(|(_, r)| r.entry_ids.contains(&entry_id))
        .map(|(i, _)| i)
        .collect()
}

fn target_of_file(path: &Path, data: &AptData) -> Target {
    if path.starts_with(&data.roots.lists_dir) {
        // A Packages/Release index file: resolved through the same
        // repo<->file attribution the Repos tab itself uses, so this agrees
        // with `AptData::repo_has_files` by construction. Unresolved (e.g.
        // `apt-get indextargets` didn't run, or didn't know this file) falls
        // back to Global rather than guessing.
        return data
            .lists
            .files
            .iter()
            .position(|f| f.path == path)
            .and_then(|file_idx| data.file_repo_of(file_idx))
            .map(|repo_idx| Target::Repos(vec![repo_idx]))
            .unwrap_or(Target::Global);
    }
    if path.starts_with(&data.roots.etc_apt) {
        // A source file (e.g. `sources.list.d/foo.list`): every repository
        // built from an entry that lives in this file. An empty source file
        // (no entries at all) correctly falls through to Global — there is
        // no repository to blame it on.
        let mut repos: Vec<usize> = data
            .sources
            .entries
            .iter()
            .filter(|e| e.file == path)
            .flat_map(|e| repos_owning_entry(data, e.id))
            .collect();
        repos.sort_unstable();
        repos.dedup();
        return if repos.is_empty() {
            Target::Global
        } else {
            Target::Repos(repos)
        };
    }
    Target::Global
}

/// Every diagnosis from a full health pass, indexed by [`target_of`] so the
/// Repos tab and the Maintenance tab can each render their slice in O(1).
pub struct Diagnostics {
    pub all: Vec<Diagnosis>,
    /// Parallel to `AptData::repos`: indices into `all` for each repository.
    pub repo_index: Vec<Vec<usize>>,
    /// Indices into `all` for diagnoses with no owning repository.
    pub global: Vec<usize>,
}

impl Diagnostics {
    /// Run every check (`checks::run` plus the global keyring audit) and
    /// classify each result via [`target_of`].
    pub fn rebuild(data: &AptData, host: &checks::HostInfo) -> Self {
        let mut all = checks::run(data, host);
        // The keyring audit has no per-repository scope to run against (a
        // keyring can be referenced by many repositories or none), so it
        // only ever contributes Global diagnoses. Keyrings live at real
        // system paths, not under `AptRoots`, hence the literal `"/"`.
        all.extend(keyrings::diagnose(&data.sources, Path::new("/")));

        let mut diagnostics = Self {
            all,
            repo_index: vec![Vec::new(); data.repos.len()],
            global: Vec::new(),
        };
        for i in 0..diagnostics.all.len() {
            diagnostics.file_one(i, data);
        }
        diagnostics
    }

    /// Classify and merge additional diagnoses (e.g. from a live `apt
    /// update` failure) into an already-built set, without re-running the
    /// full static check suite.
    pub fn merge_update_output(&mut self, diags: Vec<Diagnosis>, data: &AptData) {
        for d in diags {
            let idx = self.all.len();
            self.all.push(d);
            self.file_one(idx, data);
        }
    }

    fn file_one(&mut self, idx: usize, data: &AptData) {
        match target_of(&self.all[idx], data) {
            Target::Repos(repos) => {
                for r in repos {
                    if let Some(bucket) = self.repo_index.get_mut(r) {
                        bucket.push(idx);
                    }
                }
            }
            Target::Global => self.global.push(idx),
        }
    }

    pub fn for_repo(&self, idx: usize) -> impl Iterator<Item = &Diagnosis> {
        self.repo_index
            .get(idx)
            .into_iter()
            .flatten()
            .map(move |&i| &self.all[i])
    }

    pub fn global_diagnoses(&self) -> impl Iterator<Item = &Diagnosis> {
        self.global.iter().map(move |&i| &self.all[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apt::AptRoots;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    fn write(dir: &TempDir, rel: &str, content: &str) {
        let p = dir.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn scope_repo_maps_directly() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list",
            "deb https://example.org/ubuntu/ noble main\n",
        );
        write(&dir, "var/lib/dpkg/status", "");
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let d = Diagnosis::new(Severity::Warning, DiagCode::NeverFetched, "x", Scope::Repo(0));
        assert_eq!(target_of(&d, &data), Target::Repos(vec![0]));
    }

    #[test]
    fn scope_entry_expanding_to_two_suites_maps_to_both_repos() {
        // One deb822 entry with two `Suites:` values expands into two
        // distinct repos (RepoKey is uri+suite), both listing this entry's
        // id — the "one entry, several repos" case `Target` exists for.
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list.d/multi.sources",
            "Types: deb\n\
             URIs: https://example.org/ubuntu/\n\
             Suites: noble noble-updates\n\
             Components: main\n",
        );
        write(&dir, "var/lib/dpkg/status", "");
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        assert_eq!(data.repos.len(), 2);
        let entry_id = data.sources.entries[0].id;
        let d = Diagnosis::new(
            Severity::Warning,
            DiagCode::SignedByMissing,
            "x",
            Scope::Entry(entry_id),
        );
        let Target::Repos(mut repos) = target_of(&d, &data) else {
            panic!("expected Target::Repos");
        };
        repos.sort_unstable();
        assert_eq!(repos, vec![0, 1]);
    }

    #[test]
    fn scope_entry_with_no_owning_repo_is_global() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list",
            "deb https://example.org/ubuntu/ noble main\n",
        );
        write(&dir, "var/lib/dpkg/status", "");
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let d = Diagnosis::new(
            Severity::Warning,
            DiagCode::SignedByMissing,
            "x",
            Scope::Entry(9999),
        );
        assert_eq!(target_of(&d, &data), Target::Global);
    }

    /// `Scope::File` under `lists_dir` needs `AptData::file_repo_of` to be
    /// populated, which only happens via a real `apt-get indextargets` call
    /// (`AptData::load(.., true)`) — and that subprocess always reports the
    /// real system's `/etc/apt`, not a fixture root, so its results can
    /// never line up with a temp-dir fixture's paths. The success path is
    /// therefore only exercised for real in `tests/perf_gate.rs`; this test
    /// covers the fallback every fixture-backed test actually hits.
    #[test]
    fn scope_file_under_lists_dir_unresolved_without_mapping_is_global() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list",
            "deb https://example.org/ubuntu/ noble main\n",
        );
        write(
            &dir,
            "var/lib/apt/lists/example.org_ubuntu_dists_noble_main_binary-amd64_Packages",
            "",
        );
        write(&dir, "var/lib/dpkg/status", "");
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        assert!(!data.mapping_available());
        let path = data.lists.files[0].path.clone();
        let d = Diagnosis::new(
            Severity::Error,
            DiagCode::NotFound404 {
                url: "x".to_string(),
            },
            "x",
            Scope::File(path),
        );
        assert_eq!(target_of(&d, &data), Target::Global);
    }

    #[test]
    fn scope_file_under_etc_apt_maps_to_that_files_repos() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list.d/a.list",
            "deb https://a.example/ noble main\n",
        );
        write(
            &dir,
            "etc/apt/sources.list.d/b.list",
            "deb https://b.example/ noble main\n",
        );
        write(&dir, "var/lib/dpkg/status", "");
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let a_repo = data
            .repos
            .iter()
            .position(|r| r.key.host().contains("a.example"))
            .unwrap();
        let a_file = dir.path().join("etc/apt/sources.list.d/a.list");
        let d = Diagnosis::new(Severity::Warning, DiagCode::LegacyFormat, "x", Scope::File(a_file));
        assert_eq!(target_of(&d, &data), Target::Repos(vec![a_repo]));
    }

    #[test]
    fn scope_file_under_etc_apt_with_no_entries_is_global() {
        let dir = TempDir::new().unwrap();
        write(&dir, "etc/apt/sources.list.d/empty.list", "");
        write(&dir, "var/lib/dpkg/status", "");
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let empty_file = dir.path().join("etc/apt/sources.list.d/empty.list");
        let d = Diagnosis::new(
            Severity::Info,
            DiagCode::EmptySourceFile,
            "x",
            Scope::File(empty_file),
        );
        assert_eq!(target_of(&d, &data), Target::Global);
    }

    #[test]
    fn scope_file_outside_both_roots_is_global() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list",
            "deb https://example.org/ubuntu/ noble main\n",
        );
        write(&dir, "var/lib/dpkg/status", "");
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let d = Diagnosis::new(
            Severity::Warning,
            DiagCode::Other {
                raw: "x".to_string(),
            },
            "x",
            Scope::File(PathBuf::from("/some/unrelated/path")),
        );
        assert_eq!(target_of(&d, &data), Target::Global);
    }

    #[test]
    fn scope_keyring_and_global_are_always_global() {
        let dir = TempDir::new().unwrap();
        write(&dir, "etc/apt/sources.list", "");
        write(&dir, "var/lib/dpkg/status", "");
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();

        let kd = Diagnosis::new(
            Severity::Warning,
            DiagCode::OrphanedKeyring { global: false },
            "x",
            Scope::Keyring(PathBuf::from("/etc/apt/trusted.gpg.d/x.gpg")),
        );
        assert_eq!(target_of(&kd, &data), Target::Global);

        let gd = Diagnosis::new(
            Severity::Info,
            DiagCode::Other {
                raw: "x".to_string(),
            },
            "x",
            Scope::Global,
        );
        assert_eq!(target_of(&gd, &data), Target::Global);
    }

    #[test]
    fn rebuild_partitions_every_diagnosis_into_a_valid_bucket() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list.d/a.list",
            "deb https://a.example/ noble main\n",
        );
        write(&dir, "var/lib/dpkg/status", "");
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let host = checks::HostInfo::detect();
        let diagnostics = Diagnostics::rebuild(&data, &host);

        assert_eq!(diagnostics.repo_index.len(), data.repos.len());
        for bucket in &diagnostics.repo_index {
            for &i in bucket {
                assert!(i < diagnostics.all.len());
            }
        }
        for &i in &diagnostics.global {
            assert!(i < diagnostics.all.len());
        }
    }
}
