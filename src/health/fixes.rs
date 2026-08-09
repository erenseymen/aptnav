//! Turns a [`Diagnosis`] into a concrete, describable remedy, and applies it.
//!
//! Every fix here requires explicit user confirmation in the UI before
//! `apply` is called — this module does not decide *whether* to fix
//! something, only *how*, and always backs up the file it's about to touch
//! (see [`crate::actions::edit::backup`]).

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

use crate::actions::edit;
use crate::apt::sources::Sources;

use super::{DiagCode, Diagnosis};

/// A concrete, user-confirmable remedy for a [`Diagnosis`].
#[derive(Debug, Clone)]
pub enum Fix {
    /// Comment out (one-line) or set `Enabled: no` (deb822) on an entry.
    DisableEntry { entry_id: usize },
    /// Remove an entry's lines from its file entirely.
    RemoveEntry { entry_id: usize },
    /// Delete a file APT never reads (`.save`, `.distUpgrade`, empty source file).
    DeleteFile { path: PathBuf },
    /// Resolve and trust a missing GPG key, scoped to one entry via Signed-By.
    ResolveNoPubkey { entry_id: usize, keyid: String },
    /// Comment out the duplicate entry, keeping the first occurrence.
    CommentDuplicate { entry_id: usize },
}

impl Fix {
    pub fn describe(&self, sources: &Sources) -> String {
        match self {
            Fix::DisableEntry { entry_id } => format!(
                "Disable {}",
                sources
                    .entry(*entry_id)
                    .map(|e| e.file.display().to_string())
                    .unwrap_or_default()
            ),
            Fix::RemoveEntry { entry_id } => format!(
                "Remove entry from {}",
                sources
                    .entry(*entry_id)
                    .map(|e| e.file.display().to_string())
                    .unwrap_or_default()
            ),
            Fix::DeleteFile { path } => format!("Delete {}", path.display()),
            Fix::ResolveNoPubkey { keyid, .. } => format!("Fetch and trust key {keyid}"),
            Fix::CommentDuplicate { entry_id } => format!(
                "Comment out duplicate entry in {}",
                sources
                    .entry(*entry_id)
                    .map(|e| e.file.display().to_string())
                    .unwrap_or_default()
            ),
        }
    }
}

/// Guidance shown for problems this app cannot fix from a terminal — the
/// remedy lives outside APT's config entirely (system clock, OS trust
/// store, network).
pub fn explain_only(code: &DiagCode) -> Option<&'static str> {
    match code {
        DiagCode::ReleaseNotValidYet => Some(
            "Your system clock appears to be behind. Check it with `timedatectl status` and, \
             if `System clock synchronized` shows `no`, run `sudo timedatectl set-ntp true`.",
        ),
        DiagCode::CertVerificationFailed => Some(
            "TLS certificate verification failed. If you're behind a corporate proxy or \
             firewall that intercepts HTTPS, its CA certificate needs to be installed into the \
             system trust store: copy it to /usr/local/share/ca-certificates/ and run \
             `sudo update-ca-certificates`. If not, the mirror's certificate may genuinely be \
             invalid or expired — try a different mirror.",
        ),
        DiagCode::ConnectionFailed => Some(
            "Could not reach the repository's server. Check your network connection and DNS \
             (`resolvctl status` or `/etc/resolv.conf`), and confirm the host isn't blocked by a \
             firewall or VPN split-tunnel rule.",
        ),
        DiagCode::ConflictingDistribution => Some(
            "The repository's Release file reports a different suite/codename than expected. \
             This usually means the mirror is misconfigured or the repo has been renamed/retired \
             upstream — check the provider's documentation for the current URL.",
        ),
        _ => None,
    }
}

/// Suggest a fix for a diagnosis, if this app can perform one automatically.
/// Returns `None` for conditions that are either already actionable via a
/// simpler direct control (e.g. toggling enable/disable from the repo list)
/// or fall under [`explain_only`] instead.
pub fn plan_fix(diag: &Diagnosis) -> Option<Fix> {
    match &diag.code {
        DiagCode::EmptySourceFile | DiagCode::ClutterFile => {
            if let super::Scope::File(path) = &diag.scope {
                Some(Fix::DeleteFile { path: path.clone() })
            } else {
                None
            }
        }
        DiagCode::SuiteMismatch { .. } => match diag.scope {
            super::Scope::Repo(_) => None, // resolved to entry_id by the caller, which knows the repo's entry_ids
            _ => None,
        },
        DiagCode::DuplicateEntry => None, // needs the second entry_id; planned in `plan_fix_for_repo`
        DiagCode::NoPubkey { keyid } => match diag.scope {
            super::Scope::Repo(_) => None, // same: needs entry_id resolution by caller
            _ => Some(Fix::ResolveNoPubkey {
                entry_id: usize::MAX,
                keyid: keyid.clone(),
            }),
        },
        DiagCode::OrphanedKeyring { .. } => match &diag.scope {
            super::Scope::Keyring(path) => Some(Fix::DeleteFile { path: path.clone() }),
            _ => None,
        },
        _ => None,
    }
}

/// Whether a diagnosis is safe to include in "apply every fix" bulk
/// operations, as opposed to requiring individual review via `f`. A
/// globally-trusted keyring's blast radius (every repository, not just one)
/// means it's deliberately excluded from the bulk path even though it has an
/// automatic remedy.
pub fn bulk_safe(diag: &Diagnosis) -> bool {
    !matches!(diag.code, DiagCode::OrphanedKeyring { global: true })
}

/// Resolve a `NO_PUBKEY` key ID to an armoured public key, preferring the
/// Launchpad API for `ppa.launchpad*` URIs (returns the PPA's own signing
/// key directly, no keyserver trust-on-first-use needed) and falling back
/// to `keyserver.ubuntu.com` otherwise. The caller must show the returned
/// fingerprint and UID to the user for confirmation before calling
/// [`trust_key`] — this function only *looks up*, it never trusts anything.
#[derive(Debug, Clone)]
pub struct ResolvedKey {
    pub fingerprint: String,
    pub uid: String,
    pub armored: String,
}

pub fn resolve_launchpad_ppa_key(owner: &str, ppa_name: &str) -> Result<ResolvedKey> {
    let url = format!("https://api.launchpad.net/1.0/~{owner}/+archive/ubuntu/{ppa_name}");
    let json = curl_get(&url)?;
    let fingerprint = extract_json_string(&json, "signing_key_fingerprint")
        .context("Launchpad API response had no signing_key_fingerprint")?;
    fetch_key_from_keyserver(&fingerprint)
}

pub fn fetch_key_from_keyserver(fingerprint_or_keyid: &str) -> Result<ResolvedKey> {
    let url = format!(
        "https://keyserver.ubuntu.com/pks/lookup?op=get&options=mr&search=0x{}",
        fingerprint_or_keyid.trim_start_matches("0x")
    );
    let armored = curl_get(&url)?;
    if !armored.contains("BEGIN PGP PUBLIC KEY BLOCK") {
        anyhow::bail!("keyserver did not return a public key for {fingerprint_or_keyid}");
    }
    let (fingerprint, uid) = inspect_key(&armored)?;
    Ok(ResolvedKey {
        fingerprint,
        uid,
        armored,
    })
}

fn curl_get(url: &str) -> Result<String> {
    let output = Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location"])
        .arg(url)
        .output()
        .context("running curl")?;
    if !output.status.success() {
        anyhow::bail!(
            "curl failed for {url}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Extract `"key": "value"` from a small JSON response without a JSON
/// dependency — deliberately minimal, tolerant only of the flat shape the
/// Launchpad archive API actually returns for this one field.
fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\": \"");
    let start = json.find(&needle)? + needle.len();
    let end = json[start..].find('"')? + start;
    let val = &json[start..end];
    if val.is_empty() || val == "null" {
        None
    } else {
        Some(val.to_string())
    }
}

/// Pull the fingerprint and primary UID out of an armoured key via `gpg
/// --with-colons`, so the UI can show the user what they're about to trust.
fn inspect_key(armored: &str) -> Result<(String, String)> {
    let mut child = Command::new("gpg")
        .args(["--with-colons", "--import-options", "show-only", "--import"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("spawning gpg")?;
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(armored.as_bytes())
        .context("writing key to gpg stdin")?;
    let output = child.wait_with_output().context("waiting for gpg")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut fingerprint = String::new();
    let mut uid = String::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        match fields.first() {
            Some(&"fpr") if fingerprint.is_empty() => {
                fingerprint = fields.get(9).unwrap_or(&"").to_string();
            }
            Some(&"uid") if uid.is_empty() => {
                uid = fields.get(9).unwrap_or(&"").to_string();
            }
            _ => {}
        }
    }
    if fingerprint.is_empty() {
        anyhow::bail!("could not parse a fingerprint from the fetched key");
    }
    Ok((fingerprint, uid))
}

/// Write a resolved key to `/etc/apt/keyrings/<name>.gpg` (dearmored) via a
/// privileged write, for the caller to then reference with `Signed-By` in
/// the owning source entry. Returns the path written. Requires
/// [`crate::actions::privileges::PrivilegeSession::ensure`] to have been
/// called first — `/etc/apt/keyrings` is root-owned, so this shells out
/// through `sudo -n`, the same as every other privileged write.
pub fn dearmor_to_keyring(resolved: &ResolvedKey, name_hint: &str) -> Result<PathBuf> {
    let output = Command::new("gpg")
        .arg("--dearmor")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(resolved.armored.as_bytes())?;
            child.wait_with_output()
        })
        .context("running gpg --dearmor")?;
    if !output.status.success() {
        anyhow::bail!("gpg --dearmor failed");
    }
    let path = PathBuf::from(format!(
        "/etc/apt/keyrings/{}.gpg",
        sanitize_filename(name_hint)
    ));
    // `--dearmor` output is binary OpenPGP data, not text — the privileged
    // write script base64-encodes it regardless of content, so raw bytes
    // round-trip safely; going through a `String` here would risk lossy
    // UTF-8 substitution corrupting it before it ever reaches disk.
    let script = edit::privileged_write_script(&path, &output.stdout);
    let status = crate::actions::privileges::sudo_command("sh", &["-c", &script])
        .status()
        .context("running privileged keyring write")?;
    if !status.success() {
        anyhow::bail!("writing {} as root failed", path.display());
    }
    Ok(path)
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_string_finds_field() {
        let json = r#"{"self_link": "...", "signing_key_fingerprint": "3AE27527713D2479DCAFFD58A89D7C1B2F76304D", "name": "ppa"}"#;
        assert_eq!(
            extract_json_string(json, "signing_key_fingerprint"),
            Some("3AE27527713D2479DCAFFD58A89D7C1B2F76304D".to_string())
        );
    }

    #[test]
    fn extract_json_string_null_is_none() {
        let json = r#"{"signing_key_fingerprint": null}"#;
        assert_eq!(extract_json_string(json, "signing_key_fingerprint"), None);
    }

    #[test]
    fn sanitize_filename_strips_unsafe_chars() {
        assert_eq!(sanitize_filename("cappelikan/ppa"), "cappelikan-ppa");
    }

    #[test]
    fn explain_only_covers_release_not_valid_yet() {
        assert!(explain_only(&DiagCode::ReleaseNotValidYet).is_some());
    }

    #[test]
    fn explain_only_none_for_fixable_codes() {
        assert!(explain_only(&DiagCode::EmptySourceFile).is_none());
    }
}
