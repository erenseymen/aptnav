//! Repository health: static diagnostics, `apt-get update` failure
//! classification, keyring auditing, and auto-fix planning/execution.

pub mod checks;
pub mod fixes;
pub mod keyrings;
pub mod update;

use std::path::PathBuf;

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
