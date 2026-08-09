//! Static repository diagnostics: everything detectable from files already
//! on disk, with no network access. Runs on every load, instantly.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::apt::model::{SignedBy, SourceFormat};
use crate::apt::AptData;

use super::{DiagCode, Diagnosis, Scope, Severity};

/// Host facts the checks need: the current release's codename (to catch a
/// PPA/source still targeting a prior release after `do-release-upgrade`)
/// and the primary architecture (to catch a repo with no matching
/// `binary-<arch>` index).
#[derive(Debug, Clone)]
pub struct HostInfo {
    pub codename: Option<String>,
    pub arch: Option<String>,
}

impl HostInfo {
    pub fn detect() -> Self {
        Self {
            codename: read_os_release_codename(Path::new("/etc/os-release")),
            arch: Command::new("dpkg")
                .arg("--print-architecture")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()),
        }
    }
}

fn read_os_release_codename(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("VERSION_CODENAME=") {
            return Some(v.trim_matches('"').to_string());
        }
    }
    None
}

/// Run every static check against `data`, returning diagnoses sorted with
/// the most severe first.
pub fn run(data: &AptData, host: &HostInfo) -> Vec<Diagnosis> {
    let mut out = Vec::new();

    check_empty_and_clutter_files(data, &mut out);
    check_deprecated_trusted_gpg(&mut out);
    check_stale_lists(data, &mut out);

    for (idx, repo) in data.repos.iter().enumerate() {
        check_never_fetched(data, idx, repo, &mut out);
        check_suite_mismatch(data, idx, repo, host, &mut out);
        check_duplicate_entry(repo, idx, &mut out);
        check_missing_arch(repo, idx, host, &mut out);
    }

    for entry in &data.sources.entries {
        check_legacy_format(entry, &mut out);
        check_signed_by_missing(entry, &mut out);
        check_insecure_http(entry, &mut out);
        check_unauthenticated(entry, &mut out);
    }

    out.sort_by(|a, b| b.severity.cmp(&a.severity));
    out
}

fn check_empty_and_clutter_files(data: &AptData, out: &mut Vec<Diagnosis>) {
    let entry_files: std::collections::HashSet<&Path> = data
        .sources
        .entries
        .iter()
        .map(|e| e.file.as_path())
        .collect();

    for file in &data.sources.files {
        if !entry_files.contains(file.as_path()) {
            out.push(Diagnosis::new(
                Severity::Info,
                DiagCode::EmptySourceFile,
                format!("{} defines no source entries", file.display()),
                Scope::File(file.clone()),
            ));
        }
    }

    // Clutter: `.save`, `.distUpgrade`, and any other extension APT never
    // reads, sitting next to the sources.list.d files that do. This must be
    // `sources.list.d` specifically, not wherever the first-loaded source
    // file happens to live (`sources.list` itself is loaded first and sits
    // directly in `/etc/apt`, which would scan the wrong directory and flag
    // unrelated files like `trusted.gpg`).
    {
        let parts_dir = data.roots.etc_apt.join("sources.list.d");
        if let Ok(entries) = fs::read_dir(&parts_dir) {
            for e in entries.flatten() {
                let path = e.path();
                if !path.is_file() {
                    continue;
                }
                let ext = path.extension().and_then(|e| e.to_str());
                if !matches!(ext, Some("list") | Some("sources")) {
                    out.push(Diagnosis::new(
                        Severity::Info,
                        DiagCode::ClutterFile,
                        format!(
                            "{} is not read by APT (not a .list or .sources file)",
                            path.display()
                        ),
                        Scope::File(path),
                    ));
                }
            }
        }
    }
}

fn check_deprecated_trusted_gpg(out: &mut Vec<Diagnosis>) {
    let path = PathBuf::from("/etc/apt/trusted.gpg");
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > 0 {
            out.push(Diagnosis::new(
                Severity::Info,
                DiagCode::DeprecatedTrustedGpg,
                "/etc/apt/trusted.gpg is deprecated; keys here are trusted globally for every repository. Consider migrating each key to a per-repository Signed-By.".to_string(),
                Scope::Keyring(path),
            ));
        }
    }
}

fn check_never_fetched(
    data: &AptData,
    idx: usize,
    repo: &crate::apt::model::Repo,
    out: &mut Vec<Diagnosis>,
) {
    // Only meaningful once `apt-get indextargets` has actually resolved
    // repo↔file attribution; without it, "no files found" would just mean
    // "we didn't look", not "APT never fetched this".
    if !repo.enabled || !data.mapping_available() {
        return;
    }
    if !data.repo_has_files(idx) {
        out.push(Diagnosis::new(
            Severity::Warning,
            DiagCode::NeverFetched,
            format!(
                "{} has no cached index; run `apt update` to fetch it or check for connectivity/URL problems",
                repo.title()
            ),
            Scope::Repo(idx),
        ));
    }
}

/// A list file older than this is flagged as stale — long enough that a
/// normal update cadence never trips it, short enough to catch a repo that
/// silently stopped updating (its InRelease keeps expiring and `apt update`
/// silently skips a permanently broken source after enough failures).
const STALE_DAYS: i64 = 120;

pub fn check_stale_lists(data: &AptData, out: &mut Vec<Diagnosis>) {
    let now = SystemTime::now();
    for (fidx, f) in data.lists.files.iter().enumerate() {
        let Ok(meta) = fs::metadata(&f.path) else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        let days = age.as_secs() as i64 / 86400;
        if days >= STALE_DAYS {
            let scope = match data.file_repo_of(fidx) {
                Some(repo_idx) => Scope::Repo(repo_idx),
                None => Scope::File(f.path.clone()),
            };
            out.push(Diagnosis::new(
                Severity::Info,
                DiagCode::StaleList { days },
                format!(
                    "{} has not been refreshed in {days} days",
                    f.path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
                ),
                scope,
            ));
        }
    }
}

fn check_suite_mismatch(
    _data: &AptData,
    idx: usize,
    repo: &crate::apt::model::Repo,
    host: &HostInfo,
    out: &mut Vec<Diagnosis>,
) {
    let Some(codename) = &host.codename else {
        return;
    };
    // Only meaningful for Ubuntu-hosted archives that clearly encode a
    // codename; a repo suite equal to a *different* known Ubuntu codename
    // than the host is a strong signal of a stale PPA left over from a
    // release upgrade.
    const KNOWN_CODENAMES: &[&str] = &[
        "bionic", "focal", "jammy", "noble", "oracular", "plucky", "buster", "bullseye",
        "bookworm", "trixie",
    ];
    let suite = repo.key.suite.as_str();
    if KNOWN_CODENAMES.contains(&suite) && suite != codename {
        out.push(Diagnosis::new(
            Severity::Warning,
            DiagCode::SuiteMismatch {
                entry_suite: suite.to_string(),
                host_codename: codename.clone(),
            },
            format!(
                "{} targets '{suite}', but this system is '{codename}' — likely a leftover from a release upgrade",
                repo.title()
            ),
            Scope::Repo(idx),
        ));
    }
}

fn check_duplicate_entry(repo: &crate::apt::model::Repo, idx: usize, out: &mut Vec<Diagnosis>) {
    if repo.entry_ids.len() > 1 {
        out.push(Diagnosis::new(
            Severity::Warning,
            DiagCode::DuplicateEntry,
            format!(
                "{} is configured redundantly by {} separate source entries",
                repo.title(),
                repo.entry_ids.len()
            ),
            Scope::Repo(idx),
        ));
    }
}

fn check_missing_arch(
    repo: &crate::apt::model::Repo,
    idx: usize,
    host: &HostInfo,
    out: &mut Vec<Diagnosis>,
) {
    let Some(arch) = &host.arch else { return };
    if repo.architectures.is_empty() {
        return; // no explicit arch restriction; assumed to cover the host default
    }
    if !repo.architectures.iter().any(|a| a == arch || a == "all") {
        out.push(Diagnosis::new(
            Severity::Info,
            DiagCode::MissingHostArch,
            format!(
                "{} only lists architectures [{}], not this system's '{arch}'",
                repo.title(),
                repo.architectures.join(", ")
            ),
            Scope::Repo(idx),
        ));
    }
}

fn check_legacy_format(entry: &crate::apt::model::SourceEntry, out: &mut Vec<Diagnosis>) {
    if entry.format == SourceFormat::OneLine
        && entry.file.file_name().is_some_and(|n| n != "sources.list")
    {
        out.push(Diagnosis::new(
            Severity::Info,
            DiagCode::LegacyFormat,
            format!(
                "{} uses the legacy one-line format; deb822 (.sources) is the current standard",
                entry.file.display()
            ),
            Scope::Entry(entry.id),
        ));
    }
}

fn check_signed_by_missing(entry: &crate::apt::model::SourceEntry, out: &mut Vec<Diagnosis>) {
    if let Some(SignedBy::Path(p)) = &entry.signed_by {
        if !p.is_file() {
            out.push(Diagnosis::new(
                Severity::Error,
                DiagCode::SignedByMissing,
                format!(
                    "{} references signing key {} which does not exist",
                    entry.file.display(),
                    p.display()
                ),
                Scope::Entry(entry.id),
            ));
        }
    }
}

fn check_insecure_http(entry: &crate::apt::model::SourceEntry, out: &mut Vec<Diagnosis>) {
    if entry.uris.iter().any(|u| u.starts_with("http://")) {
        out.push(Diagnosis::new(
            Severity::Info,
            DiagCode::InsecureHttp,
            format!(
                "{} uses plain http:// instead of https://",
                entry.file.display()
            ),
            Scope::Entry(entry.id),
        ));
    }
}

fn check_unauthenticated(entry: &crate::apt::model::SourceEntry, out: &mut Vec<Diagnosis>) {
    let trusted = entry
        .options
        .get("trusted")
        .map(|v| v.eq_ignore_ascii_case("yes"))
        .unwrap_or(false);
    if trusted {
        out.push(Diagnosis::new(
            Severity::Warning,
            DiagCode::UnauthenticatedTrusted,
            format!(
                "{} sets trusted=yes, disabling signature verification entirely",
                entry.file.display()
            ),
            Scope::Entry(entry.id),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apt::AptRoots;
    use std::io::Write;
    use tempfile::TempDir;

    fn write(dir: &TempDir, rel: &str, content: &str) {
        let p = dir.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn detects_empty_source_file() {
        let dir = TempDir::new().unwrap();
        write(&dir, "etc/apt/sources.list.d/empty.sources", "");
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let host = HostInfo {
            codename: None,
            arch: None,
        };
        let diags = run(&data, &host);
        assert!(diags.iter().any(|d| d.code == DiagCode::EmptySourceFile));
    }

    #[test]
    fn detects_clutter_file() {
        // Regression test: `sources.list` must be present (as it always is
        // on a real system) and loaded *first*, so the clutter scan can't
        // accidentally key off its parent (`/etc/apt`) instead of
        // `sources.list.d` — that bug previously made the check scan the
        // wrong directory and undercount real clutter (79 -> 4 on a real
        // machine) while flagging unrelated `/etc/apt` files instead.
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list",
            "deb https://main.example/ noble main\n",
        );
        write(
            &dir,
            "etc/apt/trusted.gpg",
            "not really a gpg file, just needs to exist",
        );
        write(
            &dir,
            "etc/apt/sources.list.d/foo.list",
            "deb https://a.example/ noble main\n",
        );
        write(
            &dir,
            "etc/apt/sources.list.d/foo.list.save",
            "deb https://a.example/ jammy main\n",
        );
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let host = HostInfo {
            codename: None,
            arch: None,
        };
        let diags = run(&data, &host);
        let clutter: Vec<_> = diags
            .iter()
            .filter(|d| d.code == DiagCode::ClutterFile)
            .collect();
        assert_eq!(
            clutter.len(),
            1,
            "expected exactly foo.list.save to be flagged, got {clutter:?}"
        );
        assert!(matches!(&clutter[0].scope, Scope::File(p) if p.ends_with("foo.list.save")));
    }

    #[test]
    fn detects_suite_mismatch_after_release_upgrade() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list.d/old-ppa.list",
            "deb https://ppa.launchpad.net/x/y/ubuntu focal main\n",
        );
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let host = HostInfo {
            codename: Some("noble".to_string()),
            arch: None,
        };
        let diags = run(&data, &host);
        assert!(diags.iter().any(|d| matches!(&d.code, DiagCode::SuiteMismatch { entry_suite, host_codename } if entry_suite == "focal" && host_codename == "noble")));
    }

    #[test]
    fn no_suite_mismatch_when_matching_host() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list.d/ok.list",
            "deb https://example.org/ noble main\n",
        );
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let host = HostInfo {
            codename: Some("noble".to_string()),
            arch: None,
        };
        let diags = run(&data, &host);
        assert!(!diags
            .iter()
            .any(|d| matches!(d.code, DiagCode::SuiteMismatch { .. })));
    }

    #[test]
    fn detects_insecure_http_and_unauthenticated() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list.d/bad.list",
            "deb [trusted=yes] http://example.org/ noble main\n",
        );
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let host = HostInfo {
            codename: None,
            arch: None,
        };
        let diags = run(&data, &host);
        assert!(diags.iter().any(|d| d.code == DiagCode::InsecureHttp));
        assert!(diags
            .iter()
            .any(|d| d.code == DiagCode::UnauthenticatedTrusted));
    }

    #[test]
    fn detects_signed_by_target_missing() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list.d/bad.list",
            "deb [signed-by=/nonexistent/key.gpg] https://example.org/ noble main\n",
        );
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let host = HostInfo {
            codename: None,
            arch: None,
        };
        let diags = run(&data, &host);
        assert!(diags.iter().any(|d| d.code == DiagCode::SignedByMissing));
    }

    #[test]
    fn detects_duplicate_repo_across_two_entries() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list.d/a.list",
            "deb https://example.org/ubuntu/ noble main\n",
        );
        write(
            &dir,
            "etc/apt/sources.list.d/b.list",
            "deb https://example.org/ubuntu/ noble main\n",
        );
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let host = HostInfo {
            codename: None,
            arch: None,
        };
        let diags = run(&data, &host);
        assert!(diags.iter().any(|d| d.code == DiagCode::DuplicateEntry));
    }

    #[test]
    fn legacy_format_flagged_for_list_files_not_sources_list_itself() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "etc/apt/sources.list",
            "deb https://a.example/ noble main\n",
        );
        write(
            &dir,
            "etc/apt/sources.list.d/legacy.list",
            "deb https://b.example/ noble main\n",
        );
        let data = AptData::load(AptRoots::under(dir.path()), false).unwrap();
        let host = HostInfo {
            codename: None,
            arch: None,
        };
        let diags = run(&data, &host);
        let legacy: Vec<_> = diags
            .iter()
            .filter(|d| d.code == DiagCode::LegacyFormat)
            .collect();
        assert_eq!(legacy.len(), 1);
    }
}
