//! Parses `/etc/apt/sources.list` and `/etc/apt/sources.list.d/*.{list,sources}`
//! in both the legacy one-line format and the modern deb822 format.
//!
//! Both formats are documented in `sources.list(5)` and `deb822(5)`. Parsing
//! keeps enough position information ([`LineSpan`]) that entries can later be
//! rewritten in place (enable/disable, migrate, delete) without disturbing
//! unrelated content in the same file.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::model::{LineSpan, SignedBy, SourceEntry, SourceFormat};

/// All source entries discovered under an APT root, plus the files that were
/// present but produced no entries (empty files, pure-comment files) — these
/// still matter for the "clutter" health check.
#[derive(Debug, Default)]
pub struct Sources {
    pub entries: Vec<SourceEntry>,
    /// Every `.list`/`.sources` file seen, including ones with 0 entries.
    pub files: Vec<PathBuf>,
}

impl Sources {
    pub fn entry(&self, id: usize) -> Option<&SourceEntry> {
        self.entries.get(id)
    }
}

/// Load every source file under `etc_apt` (normally `/etc/apt`).
pub fn load(etc_apt: &Path) -> Result<Sources> {
    let mut out = Sources::default();
    let mut next_id = 0usize;

    let main_list = etc_apt.join("sources.list");
    if main_list.is_file() {
        load_one_line_file(&main_list, &mut out, &mut next_id)?;
    }

    let parts_dir = etc_apt.join("sources.list.d");
    if parts_dir.is_dir() {
        let mut paths: Vec<PathBuf> = fs::read_dir(&parts_dir)
            .with_context(|| format!("reading {}", parts_dir.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        paths.sort();

        for path in paths {
            match path.extension().and_then(|e| e.to_str()) {
                Some("sources") => load_deb822_file(&path, &mut out, &mut next_id)?,
                Some("list") => load_one_line_file(&path, &mut out, &mut next_id)?,
                // .save, .distUpgrade, and anything else is ignored here; the
                // health checker surfaces them separately as clutter.
                _ => {}
            }
        }
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// One-line format
// ---------------------------------------------------------------------------

fn load_one_line_file(path: &Path, out: &mut Sources, next_id: &mut usize) -> Result<()> {
    out.files.push(path.to_path_buf());
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

    for (lineno, line) in text.lines().enumerate() {
        if let Some(entry) = parse_one_line(path, lineno, line, next_id) {
            out.entries.push(entry);
        }
    }
    Ok(())
}

/// Parse a single physical line of one-line-format source.
///
/// A line is one of:
///   - blank or a plain comment                    -> `None`
///   - `deb [opts] uri suite [comp...]`             -> enabled entry
///   - `deb-src [opts] uri suite [comp...]`         -> enabled entry
///   - `# deb ...` / `#deb ...`                     -> disabled entry
fn parse_one_line(
    path: &Path,
    lineno: usize,
    raw_line: &str,
    next_id: &mut usize,
) -> Option<SourceEntry> {
    let trimmed = raw_line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (enabled, content) = if let Some(rest) = trimmed.strip_prefix('#') {
        (false, rest.trim_start())
    } else {
        (true, trimmed)
    };

    if !(content.starts_with("deb ")
        || content.starts_with("deb\t")
        || content == "deb"
        || content.starts_with("deb-src ")
        || content.starts_with("deb-src\t")
        || content == "deb-src")
    {
        // A genuine comment, not a disabled entry.
        return None;
    }

    let tokens = tokenize_one_line(content);
    if tokens.is_empty() {
        return None;
    }

    let deb_type = tokens[0].clone();
    let mut idx = 1;

    let mut architectures = Vec::new();
    let mut signed_by = None;
    let mut options: BTreeMap<String, String> = BTreeMap::new();

    if idx < tokens.len() && tokens[idx].starts_with('[') {
        let opts_str = &tokens[idx];
        let inner = opts_str.trim_start_matches('[').trim_end_matches(']');
        for opt in inner.split_whitespace() {
            if let Some((k, v)) = opt.split_once('=') {
                let k = k.to_ascii_lowercase();
                match k.as_str() {
                    "arch" => architectures = v.split(',').map(|s| s.to_string()).collect(),
                    "signed-by" => signed_by = Some(SignedBy::Path(PathBuf::from(v))),
                    _ => {
                        options.insert(k, v.to_string());
                    }
                }
            }
        }
        idx += 1;
    }

    if idx >= tokens.len() {
        return None; // malformed: no URI
    }
    let uri = tokens[idx].clone();
    idx += 1;

    if idx >= tokens.len() {
        return None; // malformed: no suite
    }
    let suite = tokens[idx].clone();
    idx += 1;

    let components: Vec<String> = tokens[idx..].to_vec();

    let id = *next_id;
    *next_id += 1;

    Some(SourceEntry {
        id,
        file: path.to_path_buf(),
        format: SourceFormat::OneLine,
        span: LineSpan::new(lineno, lineno + 1),
        enabled,
        types: vec![deb_type],
        uris: vec![uri],
        suites: vec![suite],
        components,
        architectures,
        signed_by,
        options,
        raw: raw_line.to_string(),
    })
}

/// Split a one-line entry into tokens, treating a `[...]` option block as one
/// token even though it contains spaces.
fn tokenize_one_line(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = s.char_indices().peekable();
    let bytes = s.as_bytes();

    while let Some(&(i, c)) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '[' {
            // Find the matching ']'.
            if let Some(close) = s[i..].find(']') {
                let end = i + close + 1;
                tokens.push(s[i..end].to_string());
                while let Some(&(j, _)) = chars.peek() {
                    if j >= end {
                        break;
                    }
                    chars.next();
                }
                continue;
            }
        }
        let start = i;
        let mut end = bytes.len();
        while let Some(&(j, c2)) = chars.peek() {
            if c2.is_whitespace() {
                end = j;
                break;
            }
            chars.next();
        }
        tokens.push(s[start..end].to_string());
    }
    tokens
}

// ---------------------------------------------------------------------------
// deb822 format
// ---------------------------------------------------------------------------

/// One raw deb822 stanza: field name (as written) -> (value, line span of the
/// value, including continuation lines).
struct Stanza {
    fields: Vec<(String, String)>,
    span: LineSpan,
    raw: String,
}

fn load_deb822_file(path: &Path, out: &mut Sources, next_id: &mut usize) -> Result<()> {
    out.files.push(path.to_path_buf());
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

    for stanza in split_deb822_stanzas(&text) {
        if let Some(entry) = parse_deb822_stanza(path, &stanza, next_id) {
            out.entries.push(entry);
        }
    }
    Ok(())
}

/// Split a deb822 file into stanzas separated by blank lines, joining folded
/// continuation lines (lines starting with whitespace) into their field.
fn split_deb822_stanzas(text: &str) -> Vec<Stanza> {
    let lines: Vec<&str> = text.lines().collect();
    let mut stanzas = Vec::new();

    let mut cur_fields: Vec<(String, String)> = Vec::new();
    let mut cur_start: Option<usize> = None;
    let mut cur_raw_lines: Vec<&str> = Vec::new();
    let mut last_content_line = 0usize;

    let flush = |fields: &mut Vec<(String, String)>,
                 start: &mut Option<usize>,
                 raw_lines: &mut Vec<&str>,
                 last: usize,
                 stanzas: &mut Vec<Stanza>| {
        if let Some(s) = start.take() {
            if !fields.is_empty() {
                stanzas.push(Stanza {
                    fields: std::mem::take(fields),
                    span: LineSpan::new(s, last + 1),
                    raw: raw_lines.join("\n"),
                });
            }
            fields.clear();
        }
        raw_lines.clear();
    };

    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            flush(
                &mut cur_fields,
                &mut cur_start,
                &mut cur_raw_lines,
                last_content_line,
                &mut stanzas,
            );
            continue;
        }
        if line.starts_with('#') {
            continue; // full-line comment; deb822 doesn't fold these into fields
        }
        if cur_start.is_none() {
            cur_start = Some(i);
        }
        cur_raw_lines.push(line);
        last_content_line = i;

        if (line.starts_with(' ') || line.starts_with('\t')) && !cur_fields.is_empty() {
            // Continuation of the previous field's value.
            let last = cur_fields.last_mut().unwrap();
            last.1.push('\n');
            last.1.push_str(line);
        } else if let Some((k, v)) = line.split_once(':') {
            cur_fields.push((k.trim().to_string(), v.trim().to_string()));
        }
        // Lines that are neither a valid `Key: value` nor a continuation are
        // silently skipped; this keeps the parser lenient on hand-edited files.
    }
    flush(
        &mut cur_fields,
        &mut cur_start,
        &mut cur_raw_lines,
        last_content_line,
        &mut stanzas,
    );
    stanzas
}

fn parse_deb822_stanza(path: &Path, stanza: &Stanza, next_id: &mut usize) -> Option<SourceEntry> {
    let get = |name: &str| -> Option<&str> {
        stanza
            .fields
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };

    let types: Vec<String> = get("Types")
        .unwrap_or("deb")
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let uris: Vec<String> = get("URIs")
        .or_else(|| get("URI"))?
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let suites: Vec<String> = get("Suites")
        .unwrap_or_default()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let components: Vec<String> = get("Components")
        .unwrap_or_default()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let architectures: Vec<String> = get("Architectures")
        .unwrap_or_default()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    let enabled = !matches!(get("Enabled"), Some(v) if v.eq_ignore_ascii_case("no"));

    let signed_by = get("Signed-By").map(|v| {
        if v.trim_start().starts_with("-----BEGIN") || v.contains('\n') {
            SignedBy::Inline
        } else {
            SignedBy::Path(PathBuf::from(v.trim()))
        }
    });

    let known = [
        "types",
        "uris",
        "uri",
        "suites",
        "components",
        "architectures",
        "enabled",
        "signed-by",
    ];
    let mut options = BTreeMap::new();
    for (k, v) in &stanza.fields {
        if !known.contains(&k.to_ascii_lowercase().as_str()) {
            options.insert(k.clone(), v.clone());
        }
    }

    if uris.is_empty() {
        return None;
    }

    let id = *next_id;
    *next_id += 1;

    Some(SourceEntry {
        id,
        file: path.to_path_buf(),
        format: SourceFormat::Deb822,
        span: stanza.span,
        enabled,
        types,
        uris,
        suites,
        components,
        architectures,
        signed_by,
        options,
        raw: stanza.raw.clone(),
    })
}

// ---------------------------------------------------------------------------
// Expansion into per-repository units
// ---------------------------------------------------------------------------

use super::model::{RepoKey, RepoUnit};

/// Expand a source entry into the cartesian product of URIs × suites ×
/// components (and, notionally, architectures — but the repo view groups by
/// component, not by arch, since a `Packages` file already covers one arch).
pub fn expand_units(entry: &SourceEntry) -> Vec<RepoUnit> {
    let mut units = Vec::new();
    if !entry.has_binary() {
        return units; // deb-src only entries have no Packages index to browse
    }
    let comps: Vec<String> = if entry.components.is_empty() {
        vec![String::new()] // flat repo, e.g. `deb URI ./` with no components
    } else {
        entry.components.clone()
    };
    let archs: Vec<String> = if entry.architectures.is_empty() {
        vec!["*".to_string()] // means "host default", resolved later
    } else {
        entry.architectures.clone()
    };
    for uri in &entry.uris {
        for suite in &entry.suites {
            for comp in &comps {
                for arch in &archs {
                    units.push(RepoUnit {
                        entry_id: entry.id,
                        key: RepoKey::new(uri.clone(), suite.clone()),
                        component: comp.clone(),
                        architecture: arch.clone(),
                    });
                }
            }
        }
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let p = dir.path().join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        p
    }

    #[test]
    fn parses_simple_one_line_entry() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "sources.list",
            "deb https://example.org/ubuntu/ noble main restricted\n",
        );
        let s = load(dir.path()).unwrap();
        assert_eq!(s.entries.len(), 1);
        let e = &s.entries[0];
        assert!(e.enabled);
        assert_eq!(e.uris, vec!["https://example.org/ubuntu/"]);
        assert_eq!(e.suites, vec!["noble"]);
        assert_eq!(e.components, vec!["main", "restricted"]);
        assert_eq!(e.format, SourceFormat::OneLine);
    }

    #[test]
    fn parses_disabled_one_line_entry() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "sources.list",
            "# deb https://example.org/ubuntu/ noble main\n",
        );
        let s = load(dir.path()).unwrap();
        assert_eq!(s.entries.len(), 1);
        assert!(!s.entries[0].enabled);
    }

    #[test]
    fn ignores_plain_comments_and_blank_lines() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "sources.list",
            "# This is a comment\n\n# Another one\n   \n",
        );
        let s = load(dir.path()).unwrap();
        assert_eq!(s.entries.len(), 0);
    }

    #[test]
    fn parses_options_with_arch_and_signed_by() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "sources.list.d/docker.list",
            "deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu noble stable\n",
        );
        let s = load(dir.path()).unwrap();
        assert_eq!(s.entries.len(), 1);
        let e = &s.entries[0];
        assert_eq!(e.architectures, vec!["amd64"]);
        assert_eq!(
            e.signed_by,
            Some(SignedBy::Path(PathBuf::from(
                "/etc/apt/keyrings/docker.asc"
            )))
        );
        assert_eq!(e.uris, vec!["https://download.docker.com/linux/ubuntu"]);
    }

    #[test]
    fn empty_file_produces_no_entries_but_is_tracked() {
        let dir = TempDir::new().unwrap();
        write(&dir, "sources.list.d/empty.list", "");
        let s = load(dir.path()).unwrap();
        assert_eq!(s.entries.len(), 0);
        assert_eq!(s.files.len(), 1);
    }

    #[test]
    fn parses_deb822_stanza() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "sources.list.d/chrome.sources",
            "X-Repolib-Name: Google Chrome\n\
             Types: deb\n\
             URIs: https://dl.google.com/linux/chrome/deb/\n\
             Suites: stable\n\
             Components: main\n\
             Architectures: amd64\n\
             Signed-By: /usr/share/keyrings/google-chrome.gpg\n",
        );
        let s = load(dir.path()).unwrap();
        assert_eq!(s.entries.len(), 1);
        let e = &s.entries[0];
        assert_eq!(e.format, SourceFormat::Deb822);
        assert!(e.enabled);
        assert_eq!(e.suites, vec!["stable"]);
        assert_eq!(
            e.options.get("X-Repolib-Name").map(|s| s.as_str()),
            Some("Google Chrome")
        );
    }

    #[test]
    fn parses_deb822_disabled_and_multi_stanza() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "sources.list.d/multi.sources",
            "Types: deb\n\
             URIs: https://a.example/repo\n\
             Suites: noble\n\
             Components: main\n\
             Enabled: no\n\
             \n\
             Types: deb\n\
             URIs: https://b.example/repo\n\
             Suites: noble\n\
             Components: main\n",
        );
        let s = load(dir.path()).unwrap();
        assert_eq!(s.entries.len(), 2);
        assert!(!s.entries[0].enabled);
        assert!(s.entries[1].enabled);
    }

    #[test]
    fn parses_deb822_inline_signed_by() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "sources.list.d/inline.sources",
            "Types: deb\n\
             URIs: https://ppa.launchpadcontent.net/foo/ppa/ubuntu/\n\
             Suites: noble\n\
             Components: main\n\
             Signed-By:\n\
             \x20-----BEGIN PGP PUBLIC KEY BLOCK-----\n\
             \x20\n\
             \x20mQINBF1oQY8BEADz\n\
             \x20-----END PGP PUBLIC KEY BLOCK-----\n",
        );
        let s = load(dir.path()).unwrap();
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].signed_by, Some(SignedBy::Inline));
    }

    #[test]
    fn expand_units_cartesian_product() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "sources.list",
            "deb https://example.org/ubuntu/ noble main restricted\n",
        );
        let s = load(dir.path()).unwrap();
        let units = expand_units(&s.entries[0]);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].component, "main");
        assert_eq!(units[1].component, "restricted");
    }

    #[test]
    fn deb_src_only_entry_has_no_units() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "sources.list",
            "deb-src https://example.org/ubuntu/ noble main\n",
        );
        let s = load(dir.path()).unwrap();
        assert!(s.entries[0].is_source_only());
        assert_eq!(expand_units(&s.entries[0]).len(), 0);
    }

    #[test]
    fn real_world_ppa_one_line_form() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "sources.list.d/hluk.list",
            "deb http://ppa.launchpad.net/hluk/copyq/ubuntu noble main\n",
        );
        let s = load(dir.path()).unwrap();
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].components, vec!["main"]);
    }
}
