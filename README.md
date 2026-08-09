# aptnav

A fast, repository-centric terminal UI for browsing and managing APT
repositories and packages on Debian/Ubuntu.

Where `apt`/`apt-cache`/`software-properties-gtk` are package-centric or
GUI-only, aptnav puts **repositories** first: browse what a repository
actually provides, see what breaks if you remove it, and fix common repo
breakage (missing GPG keys, dead PPAs after a release upgrade, unreferenced
keyrings, legacy config) from the keyboard.

## Performance

Cold start to an interactive repository list is typically **~150ms**,
including a full index of every configured repository's package list. On a
189MB / ~178k-package-stanza APT lists directory (63 repositories), this
comes from:

- `mmap` + parallel scan (`rayon`) of every `Packages` file for name/version
  only; full package details (description, dependencies, …) are parsed
  lazily, only for the package currently selected.
- Repository↔package attribution via `apt-get indextargets`, a stable APT
  interface that reports each index file's exact path and originating
  source entry directly — no need to reimplement APT's URI-to-filename
  encoding.
- Dependency resolution, pin priorities, and dry-run simulation are left to
  `apt-get`/`apt-cache` themselves (shelled out, ~50-100ms), rather than
  reimplemented.

A regression test (`tests/perf_gate.rs`) asserts cold start stays well under
budget on any real Debian/Ubuntu system it runs on.

## Screens

**Repositories** (opening screen) — "All packages", "Not from any
repository" (installed packages with no configured source — orphaned
manual installs or leftovers from removed repos), then every configured
repository with an enabled/disabled indicator and a "never fetched" warning
when applicable.

**Packages** — opened by pressing Enter on a repository (or one of the two
special entries above). Filterable, scopeable (all / installed / upgradable
/ not installed) list with a detail pane showing the full package stanza,
install state, and pin/upgrade status.

**Health** — static and live diagnostics for one repository or the whole
configuration: empty/clutter source files, stale or never-fetched indexes,
suite mismatches after a release upgrade, duplicate entries, insecure/
unauthenticated sources, missing `Signed-By` targets, the deprecated global
`trusted.gpg`, and an audit of every keyring file against what actually
references it. Live `apt update` failures (`NO_PUBKEY`, 404s, clock skew,
TLS failures, …) are classified and, where possible, auto-fixed.

## Keybindings

### Repositories
| Key | Action |
|---|---|
| `↑↓` / `j k` | move selection |
| `Enter` | open packages for the selected repository |
| `/` | filter repositories |
| `Space` | toggle enabled/disabled |
| `a` | add a repository (URL or `ppa:user/name`) |
| `x` | remove a repository's source entry |
| `e` | edit the source file (`sudo -e`) |
| `u` | run `apt update` |
| `h` | health diagnostics for the selected repository |
| `m` | migrate a legacy `.list` entry to deb822 `.sources` |
| `i` | show removal impact (installed packages available only here) |
| `?` | help |
| `q` | quit |

### Packages
| Key | Action |
|---|---|
| `↑↓` / `j k` | move selection |
| `/` | fuzzy filter |
| `Tab` | cycle scope: all / installed / upgradable / not installed |
| `i` / `r` / `p` | install / remove / purge (dry-run + confirm first) |
| `U` | upgrade |
| `v` | pick a specific version to install, across every source that has it |
| `M` | toggle auto/manual-installed marking |
| `Esc` | back |

### Health
| Key | Action |
|---|---|
| `↑↓` / `j k` | move selection |
| `f` | apply the suggested fix (with confirmation) |
| `A` | apply every fix that's safe to batch |
| `Esc` | back |

## Safety

- Every mutating action shows a **dry run** (`apt-get -s`) or an explicit
  diff before asking for confirmation.
- Every source-file edit is **backed up** first, to
  `~/.local/share/aptnav/backups/<timestamp>/` — never as more `.save`
  clutter next to the original.
- Writes are **atomic** (temp file + rename).
- The app runs as your normal user; privilege is acquired only on the first
  write via `sudo -v` and kept warm for the session. The title bar always
  shows `[read-only]` or `[elevated]`.
- Every privileged command that actually ran is echoed into the log with
  its exit status — nothing happens invisibly.
- A GPG key is never trusted without showing its fingerprint and UID first,
  and is always scoped to the one repository that needed it via `Signed-By`
  — never added to the global keyring.
- Keyring deletion (from the health screen's keyring audit) always requires
  individual confirmation; globally-trusted keys are excluded from the bulk
  "fix everything" action specifically because deleting one affects every
  repository, not just one.

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
cases like inline PGP blocks and disabled entries), the `Packages` indexer,
dpkg status parsing, Debian version comparison, `apt-get update` failure
classification against real APT error message formats, and every health
check against synthetic fixtures. `tests/perf_gate.rs` additionally
benchmarks cold start against whatever real system it runs on.
