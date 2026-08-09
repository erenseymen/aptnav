//! The APT data layer: parses on-disk state (sources, package indexes, dpkg
//! status) and assembles it into the [`AptData`] the UI renders.

pub mod dpkg;
pub mod lists;
pub mod mapping;
pub mod model;
pub mod policy;
pub mod sources;
pub mod version;

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;

use dpkg::DpkgStatus;
use lists::ListIndex;
use model::{InstallState, Repo, RepoKey};
use sources::Sources;

/// Filesystem roots APT itself would use, overridable so the app can run
/// against a fixture tree in tests (mirrors APT's `Dir::Etc` / `Dir::State`).
#[derive(Debug, Clone)]
pub struct AptRoots {
    pub etc_apt: PathBuf,
    pub lists_dir: PathBuf,
    pub dpkg_state: PathBuf, // parent of dpkg/status and apt/extended_states
}

impl Default for AptRoots {
    fn default() -> Self {
        Self {
            etc_apt: PathBuf::from("/etc/apt"),
            lists_dir: PathBuf::from("/var/lib/apt/lists"),
            dpkg_state: PathBuf::from("/var/lib"),
        }
    }
}

impl AptRoots {
    /// Build roots from a single fixture root directory, used by tests and by
    /// `--apt-root` for offline exploration: `<root>/etc/apt`,
    /// `<root>/var/lib/apt/lists`, `<root>/var/lib`.
    pub fn under(root: &Path) -> Self {
        Self {
            etc_apt: root.join("etc/apt"),
            lists_dir: root.join("var/lib/apt/lists"),
            dpkg_state: root.join("var/lib"),
        }
    }
}

/// One row of a repository's package listing: cheap enough to build for
/// every package in a repo without touching the full stanza.
#[derive(Debug, Clone)]
pub struct PackageRow {
    pub name: String,
    pub version: String, // highest version seen across this row's stub(s)
    pub install_state: InstallState,
    pub auto_installed: bool,
    pub stub: lists::StanzaRef,
}

/// Everything the UI needs, assembled once at startup (and re-assembled on
/// an explicit refresh after `apt update` or a source edit).
pub struct AptData {
    pub roots: AptRoots,
    pub sources: Sources,
    pub lists: ListIndex,
    pub dpkg: DpkgStatus,

    pub repos: Vec<Repo>,
    repo_by_key: HashMap<RepoKey, usize>,
    /// Parallel to `lists.files`: which repo (if resolved) each list file belongs to.
    file_repo: Vec<Option<usize>>,
    /// Parallel to `lists.files`: component name, for display/grouping.
    file_component: Vec<String>,

    /// name -> stub indices (into `lists.stubs`) across every repo.
    name_stubs: HashMap<String, Vec<usize>>,
    /// Installed packages with no stub in any repository.
    pub orphan_names: Vec<String>,

    pub load_time: std::time::Duration,
    pub mapping_warning: Option<String>,
    /// True once `apt-get indextargets` has successfully populated `file_repo`.
    mapping_ok: bool,
}

impl AptData {
    /// Load everything from `roots`. `run_indextargets` is a knob so callers
    /// (and tests) can skip the `apt-get` subprocess entirely when it's not
    /// meaningful (e.g. a synthetic fixture root with no real APT config).
    pub fn load(roots: AptRoots, run_indextargets: bool) -> Result<Self> {
        let t0 = Instant::now();

        let sources = sources::load(&roots.etc_apt)?;
        let lists = ListIndex::load(&roots.lists_dir)?;
        let dpkg = dpkg::load(&roots.dpkg_state)?;

        let mut data = Self {
            roots,
            sources,
            lists,
            dpkg,
            repos: Vec::new(),
            repo_by_key: HashMap::new(),
            file_repo: Vec::new(),
            file_component: Vec::new(),
            name_stubs: HashMap::new(),
            orphan_names: Vec::new(),
            load_time: std::time::Duration::default(),
            mapping_warning: None,
            mapping_ok: false,
        };

        data.build_repos();
        if run_indextargets {
            data.attach_list_files();
        } else {
            data.file_repo = vec![None; data.lists.files.len()];
            data.file_component = vec![String::new(); data.lists.files.len()];
        }
        data.build_name_index();

        data.load_time = t0.elapsed();
        Ok(data)
    }

    /// Build the repository list purely from parsed source entries — this is
    /// authoritative and complete even for repositories that have never been
    /// successfully fetched (and so would be invisible to `apt-get
    /// indextargets`, which only knows about cached Release metadata).
    fn build_repos(&mut self) {
        for entry in &self.sources.entries {
            for unit in sources::expand_units(entry) {
                let idx = *self.repo_by_key.entry(unit.key.clone()).or_insert_with(|| {
                    self.repos.push(Repo {
                        key: unit.key.clone(),
                        entry_ids: Vec::new(),
                        components: Vec::new(),
                        architectures: Vec::new(),
                        enabled: false,
                        label: None,
                    });
                    self.repos.len() - 1
                });
                let repo = &mut self.repos[idx];
                if !repo.entry_ids.contains(&entry.id) {
                    repo.entry_ids.push(entry.id);
                }
                if !unit.component.is_empty() && !repo.components.contains(&unit.component) {
                    repo.components.push(unit.component.clone());
                }
                if unit.architecture != "*" && !repo.architectures.contains(&unit.architecture) {
                    repo.architectures.push(unit.architecture.clone());
                }
                repo.enabled |= entry.enabled;
                if repo.label.is_none() {
                    repo.label = entry
                        .options
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("X-Repolib-Name"))
                        .map(|(_, v)| v.clone())
                        .or_else(|| ppa_label(&entry.uris.first().cloned().unwrap_or_default()));
                }
            }
        }
        self.repos.sort_by(|a, b| a.key.cmp(&b.key));
        self.repo_by_key = self
            .repos
            .iter()
            .enumerate()
            .map(|(i, r)| (r.key.clone(), i))
            .collect();
    }

    /// Enrich the repo list with actual on-disk `Packages` files via
    /// `apt-get indextargets`. Non-fatal on failure: the repo list built from
    /// config alone is still fully usable, just without file-level detail.
    fn attach_list_files(&mut self) {
        self.file_repo = vec![None; self.lists.files.len()];
        self.file_component = vec![String::new(); self.lists.files.len()];

        let targets = match mapping::fetch(&[]) {
            Ok(t) => t,
            Err(e) => {
                self.mapping_warning = Some(format!(
                    "Could not run `apt-get indextargets` ({e}); repository↔package \
                     attribution will be approximate."
                ));
                return;
            }
        };
        let resolved = mapping::resolve_all(&targets, &self.sources);

        // Index list files by path for O(1) lookup.
        let file_by_path: HashMap<&Path, usize> = self
            .lists
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.as_path(), i))
            .collect();

        for r in &resolved {
            let Some(&file_idx) = file_by_path.get(r.filename.as_path()) else {
                continue; // Packages file listed by apt but not present/readable on disk
            };
            let repo_idx = *self.repo_by_key.entry(r.key.clone()).or_insert_with(|| {
                // A repo apt knows about but our source parser didn't produce
                // a unit for (e.g. an architecture-only variation); add it so
                // no data is silently dropped.
                self.repos.push(Repo {
                    key: r.key.clone(),
                    entry_ids: r.entry_id.into_iter().collect(),
                    components: vec![r.component.clone()],
                    architectures: vec![r.architecture.clone()],
                    enabled: true,
                    label: None,
                });
                self.repos.len() - 1
            });
            self.file_repo[file_idx] = Some(repo_idx);
            self.file_component[file_idx] = r.component.clone();
        }
        self.mapping_ok = true;
    }

    fn build_name_index(&mut self) {
        for (i, stub) in self.lists.stubs.iter().enumerate() {
            self.name_stubs
                .entry(stub.name.clone())
                .or_default()
                .push(i);
        }
        let mut orphans: Vec<String> = self
            .dpkg
            .packages
            .values()
            .filter(|p| p.state.is_present())
            .filter(|p| !self.name_stubs.contains_key(&p.name))
            .map(|p| p.name.clone())
            .collect();
        orphans.sort();
        self.orphan_names = orphans;
    }

    // -- Queries used by the UI ------------------------------------------------

    pub fn repo(&self, idx: usize) -> Option<&Repo> {
        self.repos.get(idx)
    }

    /// True when at least one on-disk `Packages` file is attached to this
    /// repo via `apt-get indextargets`. Only meaningful when mapping ran
    /// successfully (see [`Self::mapping_warning`]); callers should treat an
    /// unresolved mapping as "unknown", not "never fetched".
    pub fn repo_has_files(&self, idx: usize) -> bool {
        self.file_repo.iter().any(|r| *r == Some(idx))
    }

    /// Whether repo↔file attribution is trustworthy for this load (i.e.
    /// `apt-get indextargets` ran and succeeded).
    pub fn mapping_available(&self) -> bool {
        self.mapping_ok
    }

    /// Which repo a given list-file index belongs to, if resolved.
    pub fn file_repo_of(&self, file_idx: usize) -> Option<usize> {
        self.file_repo.get(file_idx).copied().flatten()
    }

    /// Every package name available in a given repo, each as the row with the
    /// highest version among that repo's stub occurrences for the name.
    pub fn packages_in_repo(&self, repo_idx: usize) -> Vec<PackageRow> {
        let mut by_name: HashMap<&str, usize> = HashMap::new(); // name -> stub idx (best so far)
        for (file_idx, repo) in self.file_repo.iter().enumerate() {
            if *repo != Some(repo_idx) {
                continue;
            }
            for stub_idx in self.file_stub_indices(file_idx) {
                let stub = &self.lists.stubs[stub_idx];
                by_name
                    .entry(stub.name.as_str())
                    .and_modify(|best| {
                        if version::compare(&stub.version, &self.lists.stubs[*best].version)
                            == Ordering::Greater
                        {
                            *best = stub_idx;
                        }
                    })
                    .or_insert(stub_idx);
            }
        }
        self.rows_from(by_name.into_values())
    }

    /// All packages known anywhere: every repo's best version, plus orphans.
    pub fn all_packages(&self) -> Vec<PackageRow> {
        let best: Vec<usize> = self
            .name_stubs
            .values()
            .map(|idxs| {
                *idxs
                    .iter()
                    .max_by(|a, b| {
                        version::compare(
                            &self.lists.stubs[**a].version,
                            &self.lists.stubs[**b].version,
                        )
                    })
                    .unwrap()
            })
            .collect();
        let mut rows = self.rows_from(best.into_iter());
        rows.extend(self.orphan_rows());
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows
    }

    pub fn orphan_rows(&self) -> Vec<PackageRow> {
        self.orphan_names
            .iter()
            .filter_map(|name| {
                let pkg = self.dpkg.packages.get(name)?;
                Some(PackageRow {
                    name: name.clone(),
                    version: pkg.version.clone(),
                    install_state: pkg.state,
                    auto_installed: self.dpkg.is_auto(name),
                    stub: lists::StanzaRef {
                        file_id: usize::MAX, // sentinel: no Packages stanza backs this row
                        start: 0,
                        end: 0,
                    },
                })
            })
            .collect()
    }

    fn file_stub_indices(&self, file_idx: usize) -> Vec<usize> {
        // Stubs are stored flat, not grouped by file; for a single file's
        // worth of stanzas this is cheap since `file_id` was assigned
        // contiguously per file during indexing — but to stay correct
        // regardless of ordering, scan is used rather than assumed contiguity.
        self.lists
            .stubs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.stanza.file_id == file_idx)
            .map(|(i, _)| i)
            .collect()
    }

    fn rows_from(&self, stub_idxs: impl Iterator<Item = usize>) -> Vec<PackageRow> {
        let mut rows: Vec<PackageRow> = stub_idxs
            .map(|i| {
                let stub = &self.lists.stubs[i];
                let installed = self.dpkg.packages.get(&stub.name);
                PackageRow {
                    name: stub.name.clone(),
                    version: installed
                        .map(|p| p.version.clone())
                        .unwrap_or_else(|| stub.version.clone()),
                    install_state: installed.map(|p| p.state).unwrap_or_default(),
                    auto_installed: self.dpkg.is_auto(&stub.name),
                    stub: stub.stanza,
                }
            })
            .collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows
    }

    /// Installed packages available *only* from `repo_idx` — what would
    /// become orphaned if the repository were removed.
    pub fn removal_impact(&self, repo_idx: usize) -> Vec<PackageRow> {
        self.packages_in_repo(repo_idx)
            .into_iter()
            .filter(|row| row.install_state.is_present())
            .filter(|row| {
                self.name_stubs
                    .get(&row.name)
                    .map(|idxs| {
                        idxs.iter().all(|&i| {
                            let fid = self.lists.stubs[i].stanza.file_id;
                            self.file_repo[fid] == Some(repo_idx)
                        })
                    })
                    .unwrap_or(false)
            })
            .collect()
    }

    /// True when an installed package has a higher version available from
    /// any configured repository than the version currently installed.
    pub fn is_upgradable(&self, name: &str) -> bool {
        let Some(installed) = self
            .dpkg
            .packages
            .get(name)
            .filter(|p| p.state.is_present())
        else {
            return false;
        };
        let Some(idxs) = self.name_stubs.get(name) else {
            return false;
        };
        idxs.iter().any(|&i| {
            version::compare(&self.lists.stubs[i].version, &installed.version) == Ordering::Greater
        })
    }

    pub fn full_stanza_text(&self, stub: lists::StanzaRef) -> Option<Vec<(String, String)>> {
        if stub.file_id == usize::MAX {
            return None;
        }
        Some(self.lists.full_stanza(stub))
    }
}

/// Derive a friendly label for a Launchpad PPA URI, e.g.
/// `https://ppa.launchpadcontent.net/cappelikan/ppa/ubuntu/` -> `PPA: cappelikan/ppa`.
fn ppa_label(uri: &str) -> Option<String> {
    let rest = uri
        .strip_prefix("https://ppa.launchpadcontent.net/")
        .or_else(|| uri.strip_prefix("http://ppa.launchpad.net/"))
        .or_else(|| uri.strip_prefix("https://ppa.launchpad.net/"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches("/ubuntu");
    Some(format!("PPA: {rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    fn write(dir: &TempDir, rel: &str, content: &str) {
        let p = dir.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    fn fixture() -> TempDir {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list",
            "deb https://example.org/ubuntu/ noble main\n\
             deb https://ppa.launchpadcontent.net/someone/ppa/ubuntu/ noble main\n",
        );
        write(
            &dir,
            "var/lib/apt/lists/example.org_ubuntu_dists_noble_main_binary-amd64_Packages",
            "Package: foo\nVersion: 1.0-1\n\n\
             Package: bar\nVersion: 2.0-1\n\n",
        );
        write(
            &dir,
            "var/lib/dpkg/status",
            "Package: foo\nStatus: install ok installed\nVersion: 1.0-1\n\n\
             Package: orphaned-tool\nStatus: install ok installed\nVersion: 9.9\n",
        );
        dir
    }

    #[test]
    fn loads_without_indextargets_and_builds_repo_list_from_config() {
        let dir = fixture();
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        assert_eq!(data.repos.len(), 2);
        assert!(data
            .repos
            .iter()
            .any(|r| r.key.host().contains("example.org")));
        assert!(data
            .repos
            .iter()
            .any(|r| r.label.as_deref() == Some("PPA: someone/ppa")));
    }

    #[test]
    fn orphan_detection_finds_installed_package_with_no_stub() {
        let dir = fixture();
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        assert!(data.orphan_names.contains(&"orphaned-tool".to_string()));
        assert!(!data.orphan_names.contains(&"foo".to_string()));
    }

    #[test]
    fn all_packages_includes_repo_and_orphan_rows() {
        let dir = fixture();
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let rows = data.all_packages();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));
        assert!(names.contains(&"orphaned-tool"));
    }

    #[test]
    fn ppa_label_parses_launchpad_uri() {
        assert_eq!(
            ppa_label("https://ppa.launchpadcontent.net/cappelikan/ppa/ubuntu/"),
            Some("PPA: cappelikan/ppa".to_string())
        );
    }
}
