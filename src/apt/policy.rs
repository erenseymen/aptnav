//! Pin priorities and candidate/installed version info, via `apt-cache
//! policy`. Deliberately not reimplemented: APT's pin-priority resolution
//! (package pins, release pins, `Pin-Priority` arithmetic, architecture
//! defaults) has enough edge cases that re-deriving it from `preferences.d`
//! would be a second APT. This is called lazily, per package, only when the
//! user opens the detail pane or the version picker (~50-90 ms per call).

use std::process::Command;

/// One row of `apt-cache policy <pkg>`'s version table: a version available
/// from possibly several package files, each with its own pin priority.
#[derive(Debug, Clone)]
pub struct VersionEntry {
    pub version: String,
    pub is_installed: bool,
    /// (priority, source description e.g. "500 https://…noble/main amd64 Packages")
    pub sources: Vec<(i32, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct PackagePolicy {
    pub installed: Option<String>,
    pub candidate: Option<String>,
    pub versions: Vec<VersionEntry>,
}

/// Run `apt-cache policy <name>` and parse it.
pub fn policy_for(name: &str) -> anyhow::Result<PackagePolicy> {
    let output = Command::new("apt-cache").arg("policy").arg(name).output()?;
    Ok(parse_policy(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_policy(text: &str) -> PackagePolicy {
    let mut policy = PackagePolicy::default();
    let mut lines = text.lines().peekable();

    while let Some(line) = lines.next() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("Installed:") {
            let v = v.trim();
            if v != "(none)" {
                policy.installed = Some(v.to_string());
            }
        } else if let Some(v) = t.strip_prefix("Candidate:") {
            let v = v.trim();
            if v != "(none)" {
                policy.candidate = Some(v.to_string());
            }
        } else if t.starts_with("***")
            || (t.starts_with(|c: char| c.is_ascii_digit()) && t.contains(' '))
        {
            // Either "*** <version> <prio>" (installed marker, indent 1) or
            // "    <version> <prio>" (candidate table row, indent 5). Either
            // way this is a *version* row, distinguished from the "        <prio>
            // <source description>" rows nested under it (indent 8) by
            // indentation depth alone — trimmed, both start with a digit
            // (`8.5.0-2ubuntu10 500` vs `500 https://…`), so indentation is
            // the only reliable signal APT's own layout gives us.
            let rest = t.trim_start_matches('*').trim();
            let mut parts = rest.splitn(2, ' ');
            let version = parts.next().unwrap_or("").to_string();
            let prio: i32 = parts.next().unwrap_or("0").trim().parse().unwrap_or(0);
            let is_installed = t.starts_with("***");

            let mut sources = Vec::new();
            while let Some(next) = lines.peek() {
                let indent = next.len() - next.trim_start().len();
                let nt = next.trim();
                if nt.is_empty() || indent < 6 || !nt.starts_with(|c: char| c.is_ascii_digit()) {
                    break;
                }
                let mut p = nt.splitn(2, ' ');
                let sp: i32 = p.next().unwrap_or("0").parse().unwrap_or(0);
                let desc = p.next().unwrap_or("").trim().to_string();
                sources.push((sp, desc));
                lines.next();
            }
            let _ = prio; // top-level priority is redundant with the first source row
            policy.versions.push(VersionEntry {
                version,
                is_installed,
                sources,
            });
        }
    }
    policy
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
firefox:
  Installed: (none)
  Candidate: 1:1snap1-0ubuntu5
  Version table:
     1:1snap1-0ubuntu5 500
        500 https://mirror.domainhizmetleri.com/ubuntu noble/main amd64 Packages
";

    #[test]
    fn parses_not_installed_with_candidate() {
        let p = parse_policy(SAMPLE);
        assert_eq!(p.installed, None);
        assert_eq!(p.candidate.as_deref(), Some("1:1snap1-0ubuntu5"));
        assert_eq!(p.versions.len(), 1);
        assert_eq!(p.versions[0].sources.len(), 1);
        assert_eq!(p.versions[0].sources[0].0, 500);
    }

    const INSTALLED_SAMPLE: &str = "\
curl:
  Installed: 8.5.0-2ubuntu10.6
  Candidate: 8.5.0-2ubuntu10.6
  Version table:
 *** 8.5.0-2ubuntu10.6 500
        500 https://mirror.domainhizmetleri.com/ubuntu noble-updates/main amd64 Packages
        100 /var/lib/dpkg/status
     8.5.0-2ubuntu10 500
        500 https://mirror.domainhizmetleri.com/ubuntu noble/main amd64 Packages
";

    #[test]
    fn parses_installed_marker_and_multiple_versions() {
        let p = parse_policy(INSTALLED_SAMPLE);
        assert_eq!(p.installed.as_deref(), Some("8.5.0-2ubuntu10.6"));
        assert_eq!(p.versions.len(), 2);
        assert!(p.versions[0].is_installed);
        assert_eq!(p.versions[0].sources.len(), 2);
        assert!(!p.versions[1].is_installed);
    }
}
