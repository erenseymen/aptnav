//! Atomic, backed-up writes to files under `/etc/apt`.
//!
//! Every modification is preceded by a timestamped copy into a dedicated
//! backup directory (`~/.local/share/aptnav/backups/<timestamp>/`, *not*
//! more `.save` clutter next to the original — this machine already has 79
//! of those). Writes themselves go through a temp-file-then-rename so a
//! crash or power loss mid-write can never leave a half-written sources
//! file behind.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

/// Root directory backups are written under.
pub fn backup_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("aptnav/backups")
}

/// Copy `path` into a fresh timestamped backup directory before it's
/// modified. Returns the backup's location. A no-op (returns `Ok(None)`)
/// when `path` doesn't exist yet (e.g. writing a brand-new source file).
pub fn backup(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    let dir = backup_root().join(stamp.to_string());
    fs::create_dir_all(&dir).with_context(|| format!("creating backup dir {}", dir.display()))?;
    let dest = dir.join(path.file_name().unwrap_or_default());
    fs::copy(path, &dest)
        .with_context(|| format!("backing up {} to {}", path.display(), dest.display()))?;
    Ok(Some(dest))
}

/// Write `content` to `path` atomically: write to a temp file in the same
/// directory (so the final `rename` is same-filesystem and atomic), then
/// rename over the target. Callers needing root should instead go through
/// [`super::privileges`] + a privileged helper, since a normal-user process
/// cannot write under `/etc/apt` directly — this function is used both for
/// that privileged path (running as root via `pkexec`/`sudo -n sh -c`) and
/// for any writes to the app's own, user-owned backup directory.
pub fn write_atomic(path: &Path, content: &str) -> Result<()> {
    write_atomic_bytes(path, content.as_bytes())
}

/// Byte-oriented counterpart of [`write_atomic`], for binary content (e.g. a
/// dearmored GPG keyring) that must not be routed through `String` and risk
/// lossy UTF-8 substitution corrupting it before it ever reaches disk.
pub fn write_atomic_bytes(path: &Path, content: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent directory")?;
    let tmp = dir.join(format!(
        ".{}.aptnav.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("out")
    ));
    fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Build the shell one-liner used to perform an atomic privileged write via
/// `sudo -n sh -c '...'`: writes to a temp file then renames, all as root,
/// without ever putting plaintext content on the command line in a way a
/// shell-injection in `content` could exploit — content is passed via a
/// heredoc-safe base64 encoding rather than naive interpolation.
pub fn privileged_write_script(path: &Path, content: &[u8]) -> String {
    use std::fmt::Write as _;
    let encoded = base64_encode(content);
    let tmp = format!("{}.aptnav.tmp", path.display());
    let mut script = String::new();
    let _ = write!(
        script,
        "echo {encoded} | base64 -d > {tmp} && mv {tmp} {target}",
        tmp = shell_quote(&tmp),
        target = shell_quote(&path.to_string_lossy()),
    );
    script
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Minimal base64 encoder (standard alphabet, with padding) so
/// [`privileged_write_script`] doesn't need an extra dependency for what is
/// a small, self-contained piece of arithmetic.
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied();
        let b2 = chunk.get(2).copied();
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);
        out.push(match b1 {
            Some(b1) => ALPHABET[(((b1 & 0x0f) << 2) | (b2.unwrap_or(0) >> 6)) as usize] as char,
            None => '=',
        });
        out.push(match b2 {
            Some(b2) => ALPHABET[(b2 & 0x3f) as usize] as char,
            None => '=',
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn backup_copies_existing_file() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("sources.list");
        fs::write(&target, "original content\n").unwrap();
        std::env::set_var("XDG_DATA_HOME", dir.path());
        let backed_up = backup(&target).unwrap().unwrap();
        assert_eq!(
            fs::read_to_string(&backed_up).unwrap(),
            "original content\n"
        );
    }

    #[test]
    fn backup_of_nonexistent_file_is_none() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("does-not-exist.list");
        assert!(backup(&target).unwrap().is_none());
    }

    #[test]
    fn write_atomic_replaces_content() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("x.list");
        fs::write(&target, "old\n").unwrap();
        write_atomic(&target, "new\n").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new\n");
    }

    #[test]
    fn write_atomic_creates_new_file() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("new.sources");
        write_atomic(&target, "Types: deb\n").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "Types: deb\n");
    }

    #[test]
    fn base64_roundtrip_via_shell_would_match() {
        // Not invoking a shell in unit tests; just check the encoder against
        // known vectors from RFC 4648.
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn privileged_write_script_quotes_path_safely() {
        let script = privileged_write_script(Path::new("/etc/apt/sources.list.d/a b.list"), b"x");
        assert!(script.contains("'/etc/apt/sources.list.d/a b.list'"));
    }
}
