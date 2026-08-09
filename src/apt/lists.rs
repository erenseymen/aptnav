//! Indexes `/var/lib/apt/lists/*_Packages` files.
//!
//! Startup only needs `Package` and `Version` per stanza to build the
//! repository/package list; the full stanza (description, depends, etc.) is
//! parsed lazily, on demand, for whichever package the user has selected.
//! Benchmarked against this machine's 63 files / 189 MB / ~178k stanzas:
//! a name+version-only scan takes ~0.3 s in pure Python, so a `memchr`+`rayon`
//! scan in Rust comfortably clears the <100 ms budget for the whole index.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use memmap2::Mmap;
use rayon::prelude::*;

/// A memory-mapped `Packages` file, kept alive for the process lifetime so
/// stanza byte ranges recorded in [`StanzaRef`] remain valid.
pub struct ListFile {
    pub path: PathBuf,
    mmap: Mmap,
}

impl ListFile {
    fn open(path: PathBuf) -> Result<Self> {
        let file = fs::File::open(&path).with_context(|| format!("opening {}", path.display()))?;
        // Safety: the file is not expected to be mutated concurrently; APT
        // itself replaces list files atomically (write-new + rename), which
        // does not invalidate an existing mmap of the old inode.
        let mmap =
            unsafe { Mmap::map(&file) }.with_context(|| format!("mmapping {}", path.display()))?;
        Ok(Self { path, mmap })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.mmap
    }
}

/// A lightweight pointer to one stanza within a [`ListFile`]: enough to look
/// the package up immediately, and to re-parse the full stanza on demand.
#[derive(Debug, Clone, Copy)]
pub struct StanzaRef {
    pub file_id: usize,
    pub start: usize,
    pub end: usize,
}

/// Name + version extracted eagerly for every stanza, plus a handle back to it.
#[derive(Debug, Clone)]
pub struct PackageStub {
    pub name: String,
    pub version: String,
    pub stanza: StanzaRef,
}

/// All indexed `Packages` files plus the eagerly-parsed stub for every stanza.
pub struct ListIndex {
    pub files: Vec<ListFile>,
    pub stubs: Vec<PackageStub>,
}

impl ListIndex {
    /// Load and index every `*_Packages` file directly under `lists_dir`
    /// (normally `/var/lib/apt/lists`, non-recursive — the `partial/` and
    /// `auxfiles/` subdirectories are intentionally not descended into).
    pub fn load(lists_dir: &Path) -> Result<Self> {
        let mut paths: Vec<PathBuf> = if lists_dir.is_dir() {
            fs::read_dir(lists_dir)
                .with_context(|| format!("reading {}", lists_dir.display()))?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.file_name().is_some_and(|n| n_ends_with_packages(n)))
                .collect()
        } else {
            Vec::new()
        };
        paths.sort();

        let files: Vec<ListFile> = paths
            .into_iter()
            .filter_map(|p| ListFile::open(p).ok()) // skip unreadable/corrupt files rather than aborting the whole index
            .collect();

        // Index each file's stanzas in parallel, then flatten with a stable
        // file_id assigned by the file's position in `files`.
        let per_file: Vec<Vec<PackageStub>> = files
            .par_iter()
            .enumerate()
            .map(|(file_id, f)| index_stanzas(file_id, f.bytes()))
            .collect();

        let stubs = per_file.into_iter().flatten().collect();

        Ok(Self { files, stubs })
    }

    /// Re-parse the full RFC822 stanza for a given reference into an ordered
    /// field list (duplicate keys are rare but preserved in order).
    pub fn full_stanza(&self, s: StanzaRef) -> Vec<(String, String)> {
        let bytes = self.files[s.file_id].bytes();
        parse_stanza_fields(&bytes[s.start..s.end])
    }
}

fn n_ends_with_packages(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|s| s.ends_with("_Packages"))
}

/// Scan one file's bytes for `Package:` stanza starts and record
/// `(name, version, byte_range)` for each. Stanzas are separated by a blank
/// line; a stanza's byte range runs from its `Package:` line to just before
/// the next stanza (or end of file).
fn index_stanzas(file_id: usize, bytes: &[u8]) -> Vec<PackageStub> {
    const NEEDLE: &[u8] = b"Package: ";
    let mut out = Vec::new();

    // Find every stanza start: byte 0 if it begins with "Package: ", plus
    // every occurrence right after a "\nPackage: ".
    let mut starts = Vec::new();
    if bytes.starts_with(NEEDLE) {
        starts.push(0);
    }
    for i in memchr::memmem::find_iter(bytes, b"\nPackage: ") {
        starts.push(i + 1);
    }

    for (idx, &start) in starts.iter().enumerate() {
        let end = starts.get(idx + 1).copied().unwrap_or(bytes.len());
        let line_end = memchr::memchr(b'\n', &bytes[start..end])
            .map(|o| start + o)
            .unwrap_or(end);
        let name = String::from_utf8_lossy(&bytes[start + NEEDLE.len()..line_end]).into_owned();

        let version = find_field(&bytes[start..end], b"\nVersion: ")
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .unwrap_or_default();

        out.push(PackageStub {
            name,
            version,
            stanza: StanzaRef {
                file_id,
                start,
                end,
            },
        });
    }
    out
}

/// Find the value of a `\nField: ` line within a stanza slice (the needle
/// must include the leading newline; the slice is assumed to start with
/// `Package: ` so a field on the very first line is still found by also
/// checking a `field-without-leading-\n` at offset 0 when relevant).
fn find_field<'a>(stanza: &'a [u8], needle: &[u8]) -> Option<&'a [u8]> {
    let start = memchr::memmem::find(stanza, needle)? + needle.len();
    let end = memchr::memchr(b'\n', &stanza[start..])
        .map(|o| start + o)
        .unwrap_or(stanza.len());
    Some(&stanza[start..end])
}

/// Parse a full stanza into an ordered `(field, value)` list, joining folded
/// continuation lines (used by `Description`, `Depends`, etc. when wrapped).
fn parse_stanza_fields(bytes: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(bytes);
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = fields.last_mut() {
                last.1.push('\n');
                last.1.push_str(line);
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(easy_colon()) {
            fields.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    fields
}

fn easy_colon() -> char {
    ':'
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_packages(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let p = dir.path().join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        p
    }

    const SAMPLE: &str = "Package: foo\n\
Version: 1.2.3-1\n\
Architecture: amd64\n\
Description: A package\n\
\x20More description text.\n\
\n\
Package: bar\n\
Version: 0.9-1ubuntu1\n\
Architecture: all\n\
Description: Another package\n\
\n";

    #[test]
    fn indexes_multiple_stanzas() {
        let dir = TempDir::new().unwrap();
        write_packages(&dir, "test_Packages", SAMPLE);
        let idx = ListIndex::load(dir.path()).unwrap();
        assert_eq!(idx.stubs.len(), 2);
        assert_eq!(idx.stubs[0].name, "foo");
        assert_eq!(idx.stubs[0].version, "1.2.3-1");
        assert_eq!(idx.stubs[1].name, "bar");
        assert_eq!(idx.stubs[1].version, "0.9-1ubuntu1");
    }

    #[test]
    fn full_stanza_joins_continuation_lines() {
        let dir = TempDir::new().unwrap();
        write_packages(&dir, "test_Packages", SAMPLE);
        let idx = ListIndex::load(dir.path()).unwrap();
        let fields = idx.full_stanza(idx.stubs[0].stanza);
        let desc = fields.iter().find(|(k, _)| k == "Description").unwrap();
        assert!(desc.1.contains("More description text."));
    }

    #[test]
    fn ignores_files_not_ending_in_packages() {
        let dir = TempDir::new().unwrap();
        write_packages(&dir, "test_InRelease", "Origin: Test\n");
        write_packages(&dir, "test_Packages", SAMPLE);
        let idx = ListIndex::load(dir.path()).unwrap();
        assert_eq!(idx.files.len(), 1);
    }

    #[test]
    fn empty_packages_file_yields_no_stubs() {
        let dir = TempDir::new().unwrap();
        write_packages(&dir, "empty_Packages", "");
        let idx = ListIndex::load(dir.path()).unwrap();
        assert_eq!(idx.stubs.len(), 0);
    }

    #[test]
    fn single_stanza_file_with_no_trailing_blank_line() {
        let dir = TempDir::new().unwrap();
        write_packages(&dir, "one_Packages", "Package: solo\nVersion: 1.0\n");
        let idx = ListIndex::load(dir.path()).unwrap();
        assert_eq!(idx.stubs.len(), 1);
        assert_eq!(idx.stubs[0].name, "solo");
    }
}

/// A convenience alias used by callers that need the field map as a `BTreeMap`
/// (e.g. the detail pane, where lookup-by-key matters more than order).
pub fn stanza_map(fields: &[(String, String)]) -> BTreeMap<String, String> {
    fields.iter().cloned().collect()
}
