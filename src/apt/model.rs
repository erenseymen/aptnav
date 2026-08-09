//! Core data types shared across the APT data layer.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

/// Which on-disk syntax a source entry was written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    /// Legacy one-line `deb http://... suite components` syntax.
    OneLine,
    /// Modern deb822 `.sources` stanza syntax.
    Deb822,
}

impl fmt::Display for SourceFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceFormat::OneLine => write!(f, "one-line"),
            SourceFormat::Deb822 => write!(f, "deb822"),
        }
    }
}

/// How a source entry declares its signing key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignedBy {
    /// A path to a keyring file on disk.
    Path(PathBuf),
    /// An inline armoured key embedded in a deb822 stanza.
    Inline,
}

/// The region of a file an entry occupies, so edits can target it precisely.
///
/// Lines are 0-based and the range is half-open: `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSpan {
    pub start: usize,
    pub end: usize,
}

impl LineSpan {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

/// A single entry as written in `sources.list` or a file under `sources.list.d`.
///
/// One entry may expand into several [`RepoUnit`]s, because deb822 permits
/// multiple URIs, suites and components in a single stanza.
#[derive(Debug, Clone)]
pub struct SourceEntry {
    /// Index of this entry in [`Sources::entries`], used as a stable handle.
    pub id: usize,
    pub file: PathBuf,
    pub format: SourceFormat,
    pub span: LineSpan,
    /// False when the entry is commented out (one-line) or `Enabled: no` (deb822).
    pub enabled: bool,
    /// `deb` and/or `deb-src`.
    pub types: Vec<String>,
    pub uris: Vec<String>,
    pub suites: Vec<String>,
    pub components: Vec<String>,
    /// Explicit `arch=` / `Architectures:`; empty means "whatever APT defaults to".
    pub architectures: Vec<String>,
    pub signed_by: Option<SignedBy>,
    /// Remaining options (`trusted`, `lang`, `target`, …), lowercased keys.
    pub options: BTreeMap<String, String>,
    /// The verbatim text of the entry, preserved for display and rewriting.
    pub raw: String,
}

impl SourceEntry {
    /// True when the entry ships binary packages (as opposed to `deb-src` only).
    pub fn has_binary(&self) -> bool {
        self.types.iter().any(|t| t == "deb")
    }

    /// True when the entry only provides source packages.
    pub fn is_source_only(&self) -> bool {
        !self.types.is_empty() && !self.has_binary()
    }
}

/// A concrete archive: one URI and suite pair drawn from a source entry.
///
/// This is the granularity the repository list displays, because it is what a
/// user thinks of as "a repository" and what `apt-cache policy` reports.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepoKey {
    pub uri: String,
    pub suite: String,
}

impl RepoKey {
    pub fn new(uri: impl Into<String>, suite: impl Into<String>) -> Self {
        Self {
            uri: normalize_uri(&uri.into()),
            suite: suite.into(),
        }
    }

    /// A compact human label, e.g. `download.docker.com/linux/ubuntu noble`.
    pub fn display(&self) -> String {
        format!("{} {}", strip_scheme(&self.uri), self.suite)
    }

    /// The host portion of the URI, used for grouping and heuristics.
    pub fn host(&self) -> &str {
        let s = strip_scheme(&self.uri);
        s.split('/').next().unwrap_or(s)
    }
}

/// Strip the `scheme://` prefix from a URI for display purposes.
pub fn strip_scheme(uri: &str) -> &str {
    match uri.find("://") {
        Some(i) => &uri[i + 3..],
        None => uri,
    }
}

/// Normalise a repository URI so the same archive written two ways compares equal.
///
/// APT itself ignores a trailing slash, so `https://example.org/deb/` and
/// `https://example.org/deb` are the same repository and must not show up as two
/// rows (or, worse, be missed by the duplicate-entry check).
pub fn normalize_uri(uri: &str) -> String {
    uri.trim().trim_end_matches('/').to_string()
}

/// One acquirable index file: a single `Packages` file on the mirror.
///
/// The cartesian product of an entry's URIs, suites, components and
/// architectures. Each unit corresponds to at most one file in
/// `/var/lib/apt/lists`.
#[derive(Debug, Clone)]
pub struct RepoUnit {
    pub entry_id: usize,
    pub key: RepoKey,
    pub component: String,
    pub architecture: String,
}

/// A repository as presented in the UI, aggregating every component and
/// architecture that a single source entry provides for one URI/suite pair.
#[derive(Debug, Clone)]
pub struct Repo {
    pub key: RepoKey,
    /// Source entries producing this repository. More than one means the entry
    /// is duplicated across files, which APT warns about.
    pub entry_ids: Vec<usize>,
    pub components: Vec<String>,
    pub architectures: Vec<String>,
    pub enabled: bool,
    /// Optional friendly name (deb822 `X-Repolib-Name`, or derived for PPAs).
    pub label: Option<String>,
}

impl Repo {
    /// Preferred display name: the friendly label when present, else URI + suite.
    pub fn title(&self) -> String {
        match &self.label {
            Some(l) => format!("{} ({})", l, self.key.display()),
            None => self.key.display(),
        }
    }
}

/// Where a package came from, as shown in the package list and detail pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Provided by a configured repository.
    Repo(usize),
    /// Installed but present in no configured repository.
    None,
}

/// Installation state of a package on this system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstallState {
    #[default]
    NotInstalled,
    Installed,
    /// Removed but configuration files remain (`deinstall`/`rc` in dpkg terms).
    ConfigFiles,
    /// dpkg reports a broken or half-configured state.
    Broken,
}

impl InstallState {
    pub fn is_present(self) -> bool {
        matches!(self, InstallState::Installed | InstallState::Broken)
    }
}
