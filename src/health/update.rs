//! Runs `apt-get update` and classifies its stderr into [`Diagnosis`]es.
//!
//! The classifier is a pure function over captured text so it can be tested
//! against real APT error messages without a network round-trip. The
//! messages matched here are APT's own, stable wording (unchanged across
//! recent APT releases on Debian/Ubuntu) for the handful of failure modes
//! that account for nearly all "why won't my repo update" reports:
//! `NO_PUBKEY`, 404s, clock-skew (`Release file is not valid yet` /
//! `has expired`), TLS certificate failures, `Conflicting distribution`
//! (suite/codename mismatch), and generic connection failures.

use std::path::PathBuf;

use super::{DiagCode, Diagnosis, Scope, Severity};

/// Running `apt-get update` itself lives in [`crate::actions::apt::update`],
/// which goes through the app's privileged-session machinery (refreshing
/// `/var/lib/apt/lists` requires root); this module only classifies the
/// resulting stderr.
///
/// Classify every line of `apt-get update` stderr into diagnoses. Unmatched
/// `W:`/`E:` lines are kept as [`DiagCode::Other`] so nothing is silently
/// dropped, just not specially handled.
pub fn classify(stderr: &str) -> Vec<Diagnosis> {
    let mut out = Vec::new();
    for line in stderr.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(d) = classify_line(line) {
            out.push(d);
        }
    }
    out
}

fn classify_line(line: &str) -> Option<Diagnosis> {
    let severity = if line.starts_with("E:") {
        Severity::Error
    } else if line.starts_with("W:") {
        Severity::Warning
    } else {
        return None;
    };

    let url = extract_url(line);
    let scope = url
        .as_ref()
        .and_then(|u| repo_host(u))
        .map(Scope::File)
        .unwrap_or(Scope::Global);

    let code = if let Some(keyid) = extract_no_pubkey(line) {
        DiagCode::NoPubkey { keyid }
    } else if line.contains("404") && line.contains("Not Found") {
        DiagCode::NotFound404 {
            url: url.clone().unwrap_or_default(),
        }
    } else if line.contains("Release file") && line.contains("is not valid yet") {
        DiagCode::ReleaseNotValidYet
    } else if line.contains("Release") && (line.contains("expired") || line.contains("has expired"))
    {
        DiagCode::ReleaseExpired
    } else if line.contains("certificate verification failed")
        || line.contains("SSL certificate problem")
        || line.contains("server certificate verification failed")
    {
        DiagCode::CertVerificationFailed
    } else if line.contains("Conflicting distribution") {
        DiagCode::ConflictingDistribution
    } else if line.contains("Could not connect")
        || line.contains("Could not resolve")
        || line.contains("Connection failed")
        || line.contains("Temporary failure resolving")
    {
        DiagCode::ConnectionFailed
    } else {
        DiagCode::Other {
            raw: line.to_string(),
        }
    };

    Some(Diagnosis::new(severity, code, line.to_string(), scope))
}

/// Pull the first `http(s)://...` token out of an apt error line.
fn extract_url(line: &str) -> Option<String> {
    let start = line.find("http://").or_else(|| line.find("https://"))?;
    let rest = &line[start..];
    let end = rest.find([' ', '\'', '"']).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn repo_host(url: &str) -> Option<PathBuf> {
    let without_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = without_scheme.split('/').next()?;
    Some(PathBuf::from(host))
}

/// `NO_PUBKEY` lines look like:
/// `W: GPG error: https://example.org noble InRelease: The following signatures couldn't be verified because the public key is not available: NO_PUBKEY 871920D1991BC93C`
fn extract_no_pubkey(line: &str) -> Option<String> {
    let idx = line.find("NO_PUBKEY")?;
    line[idx + "NO_PUBKEY".len()..]
        .split_whitespace()
        .next()
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_no_pubkey() {
        let line =
            "W: GPG error: https://ppa.launchpadcontent.net/foo/ppa/ubuntu noble InRelease: \
                    The following signatures couldn't be verified because the public key is not \
                    available: NO_PUBKEY 871920D1991BC93C";
        let diags = classify(line);
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].code,
            DiagCode::NoPubkey {
                keyid: "871920D1991BC93C".to_string()
            }
        );
    }

    #[test]
    fn classifies_404() {
        let line = "E: Failed to fetch https://example.org/dists/noble/InRelease  404  Not Found [IP: 1.2.3.4 443]";
        let diags = classify(line);
        assert_eq!(diags.len(), 1);
        assert!(
            matches!(&diags[0].code, DiagCode::NotFound404 { url } if url.starts_with("https://example.org"))
        );
    }

    #[test]
    fn classifies_release_not_valid_yet() {
        let line = "E: Release file for https://example.org/dists/noble/InRelease is not valid yet (invalid for another 3h 12min 4s). Updates for this repository will not be applied.";
        let diags = classify(line);
        assert_eq!(diags[0].code, DiagCode::ReleaseNotValidYet);
    }

    #[test]
    fn classifies_expired_release() {
        let line = "E: Release file for https://example.org/dists/noble/InRelease is expired (invalid since 10d 2h 14min 8s)";
        let diags = classify(line);
        assert_eq!(diags[0].code, DiagCode::ReleaseExpired);
    }

    #[test]
    fn classifies_cert_failure() {
        let line = "E: Failed to fetch https://example.org/dists/noble/InRelease  server certificate verification failed. CAfile: none CRLfile: none";
        let diags = classify(line);
        assert_eq!(diags[0].code, DiagCode::CertVerificationFailed);
    }

    #[test]
    fn classifies_conflicting_distribution() {
        let line = "E: Conflicting distribution: https://example.org/dists/noble InRelease (expected noble but got focal)";
        let diags = classify(line);
        assert_eq!(diags[0].code, DiagCode::ConflictingDistribution);
    }

    #[test]
    fn classifies_connection_failure() {
        let line = "W: Failed to fetch https://example.org/dists/noble/InRelease  Temporary failure resolving 'example.org'";
        let diags = classify(line);
        assert_eq!(diags[0].code, DiagCode::ConnectionFailed);
    }

    #[test]
    fn ignores_non_diagnostic_lines() {
        let text = "Hit:1 https://example.org noble InRelease\nReading package lists...";
        assert_eq!(classify(text).len(), 0);
    }

    #[test]
    fn unmatched_error_falls_back_to_other() {
        let line = "E: Some completely novel apt error nobody has seen before";
        let diags = classify(line);
        assert!(matches!(&diags[0].code, DiagCode::Other { raw } if raw == line));
    }
}
