# aptnav

A terminal UI for browsing and managing APT packages and repositories on
Debian/Ubuntu — a full-featured package manager (multi-select, hold, sort,
mouse) that is also, distinctively, a **repository health tool**: browse what
a repository actually provides, see what breaks if you remove it, and fix
common repo breakage (missing GPG keys, dead PPAs after a release upgrade,
unreferenced keyrings, legacy config) from the keyboard.

## Performance

Cold start to an interactive package list — including a full index of every
configured repository's package list — is typically **~250ms** on a 189MB /
~178k-package-stanza APT lists directory (65 repositories); building the
full "All packages" row set on top of that (the Packages tab's opening view)
adds another **~180ms**. This comes from:

- `mmap` + parallel scan (`rayon`) of every `Packages` file for name/version
  only; full package details (description, dependencies, …) are parsed
  lazily, only for the package currently selected.
- Repository↔package attribution via `apt-get indextargets`, a stable APT
  interface that reports each index file's exact path and originating
  source entry directly — no need to reimplement APT's URI-to-filename
  encoding.
- Dependency resolution, pin priorities, and dry-run simulation are left to
  `apt-get`/`apt-cache` themselves (shelled out), rather than reimplemented.

A regression test (`tests/perf_gate.rs`) asserts cold start — both the raw
index and the fully-built package table — stays well under budget on any
real Debian/Ubuntu system it runs on.

## Tabs

**Packages** (opening tab) — every package on the system, in a sortable
table (click a column header, or `S`, to sort by name/version/state).
Filterable and scopeable (all / installed / upgradable / not installed),
with checkbox multi-select for bulk install/remove/purge/upgrade, hold/unhold
(`apt-mark`), phased-update-aware upgrades (force/skip/cancel when a rollout
is holding a package back), and a detail pane showing the full package
stanza plus install/hold/essential/phased status.

**Repos** — the repository list side by side with that repository's health
diagnoses (empty/clutter source files, stale or never-fetched indexes, suite
mismatches after a release upgrade, duplicate entries, insecure/
unauthenticated sources, missing `Signed-By` targets), with repo management
(add/remove/enable/disable/edit/migrate) and a removal-impact check. Press
`Enter` on a repository to drill into its packages inline, with the full
Packages-tab feature set.

**Maintenance** — the diagnoses that aren't tied to one repository (the
deprecated global `trusted.gpg`, orphaned keyrings, live `apt update`
failures that couldn't be attributed to a source), plus package upkeep:
autoremove, cache cleanup (`clean`/`autoclean`), and purging residual config
files left behind by packages removed outside aptnav.

`Tab`/`Shift-Tab` or `1`/`2`/`3` switch tabs from anywhere; `u` runs
`apt update` from anywhere.

## Keybindings

### Global
| Key | Action |
|---|---|
| `Tab` / `Shift-Tab` | next / previous tab |
| `1` `2` `3` | jump to Packages / Repos / Maintenance |
| `u` | run `apt update` |
| `T` | toggle light/dark theme |
| `?` | help |
| `q` | quit |

### Packages (and the Repos tab's drill-down view)
| Key | Action |
|---|---|
| `↑↓` / `j k` | move cursor |
| `/` | fuzzy filter |
| `Space` | toggle selection of the cursor row |
| `a` / `A` | select / deselect every filtered package |
| `Ctrl-A` | clear the entire selection |
| `s` | cycle scope: all / installed / upgradable / not installed |
| `S` | cycle column sort: name / version / state, each asc/desc |
| `i` / `r` / `p` | install / remove / purge (selection, else cursor row) |
| `U` | upgrade (selection, else cursor row); offers force/skip/cancel if a phased rollout is holding anything back |
| `H` | toggle hold / unhold |
| `M` | toggle auto/manual-installed marking |
| `v` | pick a specific version to install, across every source that has it |
| mouse | click a column header to sort, click a row to select it, click the checkbox to toggle, wheel to scroll |

Essential packages can never be removed/purged; held packages are skipped
(with a note) on a bulk install/upgrade rather than blocking the whole
batch.

### Repos
| Key | Action |
|---|---|
| `↑↓` / `j k` | move selection in the focused pane |
| `←→` / `h l` | switch focus: repo list ↔ diagnostics |
| `Enter` | drill into the selected repository's packages |
| `/` | filter repositories |
| `Space` | toggle enabled/disabled |
| `a` | add a repository (URL or `ppa:user/name`) |
| `x` | remove a repository's source entry |
| `e` | edit the source file (`sudo -e`) |
| `m` | migrate a legacy `.list` entry to deb822 `.sources` |
| `d` | show removal impact (installed packages available only here) |
| `f` | apply the selected diagnosis's fix |
| `A` | apply every auto-fixable diagnosis for this repository |
| `Esc` (in drill-down) | back to the repository list |

### Maintenance
| Key | Action |
|---|---|
| `↑↓` / `j k` | move selection in the focused pane |
| `←→` / `h l` | switch focus: diagnostics ↔ tasks |
| `f` | apply the selected diagnosis's fix |
| `A` | apply every auto-fixable diagnosis |
| `Enter` | run the selected task |
| `c` | run autoremove, if there's anything to remove |
| `C` | clean the package cache, if there's anything cached |

## Safety

- Every mutating action shows a **dry run** (`apt-get -s`) or an explicit
  diff before asking for confirmation, listing every package a bulk action
  will touch — including ones outside the current filter/scope.
- Every source-file edit is **backed up** first, to
  `~/.local/share/aptnav/backups/<timestamp>/` — never as more `.save`
  clutter next to the original.
- Writes are **atomic** (temp file + rename).
- The app runs as your normal user; privilege is acquired only on the first
  write via `sudo -v` and kept warm for the session. The title bar always
  shows `[read-only]` or `[elevated]`.
- A GPG key is never trusted without showing its fingerprint and UID first,
  and is always scoped to the one repository that needed it via `Signed-By`
  — never added to the global keyring.
- Keyring deletion (from the Maintenance tab's keyring audit) always
  requires individual confirmation; globally-trusted keys are excluded from
  the bulk "fix everything" action specifically because deleting one affects
  every repository, not just one.

## Usage

```
aptnav [OPTIONS]

OPTIONS:
    --apt-root <DIR>   Read APT state from <DIR>/etc/apt, <DIR>/var/lib/apt/lists
                        and <DIR>/var/lib/dpkg instead of the real system.
                        Read-only exploration of a fixture tree; not for
                        driving privileged actions.
    -h, --help         Print help and exit.
```

Theme: auto-detected from the terminal background (OSC 11), overridable with
`APTNAV_THEME=dark|light` or the `T` key at runtime.

## Building

```
cargo build --release
./target/release/aptnav
```

Produce an installable `.deb` with [`cargo-deb`](https://github.com/kornelski/cargo-deb):

```
cargo install cargo-deb
cargo deb
```

## Testing

```
cargo test
```

Unit tests cover the sources parser (deb822 + one-line, including edge
cases like inline PGP blocks and disabled entries), the `Packages` indexer
(including `Essential`/`Phased-Update-Percentage` extraction), dpkg status
parsing (including hold state), Debian version comparison, `apt-get update`
failure classification against real APT error message formats, every health
check against synthetic fixtures, the repo/global diagnosis split, column
sorting, multi-select semantics, and mouse hit-testing. `tests/perf_gate.rs`
additionally benchmarks cold start against whatever real system it runs on.

## What's next

`TODO.md` tracks features deferred out of this pass — a Transactions tab
with undo/redo, a richer filter query language, mirror fetch/test/apply,
export/import, favorites, and background `apt update` — each with enough
design detail to pick up without re-deriving it from scratch.
