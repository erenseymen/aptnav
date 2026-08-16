# TODO

Deferred work, recorded in enough detail to execute without re-deriving the
research behind it.

Much of this is informed by reading [`aptui`](https://github.com/mexirica/aptui),
a mature Go/Bubble Tea APT TUI that already ships most of these features. Where
its implementation has a bug worth not repeating, that bug is written down here
under **Pitfalls** — those entries are the most valuable part of this file.

Every entry uses the same six headings:

1. **Why deferred**
2. **Design sketch** — types and signatures, not prose
3. **Files to touch**
4. **Dependencies required**
5. **Pitfalls observed in `aptui`**
6. **Acceptance**

---

## 1. Transactions tab: history + undo/redo

**Why deferred.** The journal has to survive across runs, which means a
schema'd file on disk. The project currently has no `serde` and hand-rolls its
only JSON reader (`health/fixes.rs:191`); a growing, versioned journal is the
worst possible candidate for a hand-rolled parser. Blocked on the decision to
take `serde` + `serde_json`.

**Design sketch.**

```rust
// src/history/mod.rs  (new module)

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionChange {
    pub package: String,
    pub from: Option<String>,   // None = was not installed
    pub to: Option<String>,     // None = removed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TxKind { Install, Remove, Purge, Upgrade, Autoremove, ConfigEdit }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub id: u64,
    pub kind: TxKind,
    pub timestamp: SystemTime,
    pub success: bool,
    pub changes: Vec<VersionChange>,
    /// The dry-run plan the user actually confirmed, verbatim.
    pub planned: Vec<String>,
    /// Command line + exit status, from `LoggedCommand`.
    pub command: String,
    /// For ConfigEdit: the backup directory `edit::backup` produced.
    pub backup: Option<PathBuf>,
}

pub struct Journal { path: PathBuf, entries: Vec<Transaction>, next_id: u64 }
impl Journal {
    pub fn load() -> Self;                       // missing/corrupt -> empty, but WARN
    pub fn record(&mut self, tx: Transaction) -> Result<()>;   // atomic write
    pub fn all(&self) -> &[Transaction];         // newest first
    pub fn prune(&mut self, keep: usize);        // ring, unlike aptui
}

/// Undo strength: version snapshot (recommended). See Pitfalls.
pub fn plan_undo(tx: &Transaction, data: &AptData) -> UndoPlan;
pub enum UndoPlan {
    Reinstall(Vec<(String, String)>),   // pkg=exact-old-version
    Remove(Vec<String>),
    RestoreConfig { path: PathBuf, from_backup: PathBuf },
    Impossible(&'static str),           // e.g. purge lost the config files
}
```

**Recommended undo strength: version snapshot, not aptui's inverse-command
model.** The data is already free — `actions::apt::dry_run` (`actions/apt.rs:49`)
runs before every mutation and reports every affected package, and the old
versions are already in memory in `DpkgStatus` (`apt/dpkg.rs:25`). Recording
`(package, old_version, new_version)` triples therefore costs nothing extra and
buys the one thing aptui cannot do: **undoing a bulk upgrade**. Undo executes
`apt-get install --allow-downgrades pkg=<old> ...`. When an old version is no
longer in any repository, report the partial failure honestly instead of
silently installing the current candidate.

**Files to touch.**
- `src/ui/app.rs:392` `execute_action` — **the single funnel every mutation
  already passes through.** One `Journal::record` call here covers every action
  in the app; this is why undo is tractable at all. Do not scatter recording.
- `src/ui/app.rs:486` `log_command` — already captures command + exit status.
- `src/ui/app.rs` `Tab::ALL`, `Tab::title`, `draw_body`'s match, `handle_key`'s
  dispatch match — the four places a new tab variant must be added. The tab bar
  is generated from `Tab::ALL`, so the bar, its mouse hit-rects and the numeric
  jump keys all follow automatically.
- `src/actions/edit.rs:17` `backup_root` → `~/.local/share/aptnav/backups`.
  **Backups are already being written before every config edit and nothing ever
  reads them** — undoing a source-file edit is `repos::rewrite_span` back to the
  matching backup, no new machinery needed.
- New `src/ui/transactions.rs` for the tab body (list + detail panes, filterable
  with the existing `Filter`).

**Dependencies required.** `serde = { version = "1", features = ["derive"] }`,
`serde_json = "1"`. Write via the already-present `edit::write_atomic`
(`actions/edit.rs:49`), which is currently unused outside tests.

**Pitfalls observed in `aptui`.**
- Bulk `upgrade` is refused outright by undo ("use the version selector to
  downgrade") because no version info was recorded for it. The snapshot design
  above exists specifically to fix this.
- `purge` is missing from its undo mapping table and falls through a `default`
  arm to `install` — so "undoing" a purge silently reinstalls without the config
  files it destroyed. Model this as `UndoPlan::Impossible` and say so.
- Undoing an `install` reinstalls with no version constraint, pulling the
  current candidate rather than what was there before.
- Corrupt JSON is swallowed (`_ = json.Unmarshal(...)`) and the entire history is
  silently lost. Warn loudly and keep the corrupt file aside.
- The journal has no cap and no rotation; it grows forever. Hence `prune`.
- Recording happens per-command, so a batch that fans out to several commands can
  produce several transactions. Record one per confirmed user action.

**Acceptance.** Unit tests for `plan_undo` covering: install → remove; upgrade →
downgrade to the exact recorded version; purge → `Impossible`; a package whose
recorded old version is no longer available → partial-failure report; a config
edit → restore from the recorded backup path. Round-trip test for `Journal`
save/load, plus an explicit corrupt-file test asserting the warning path.

---

## 2. Rich filter query language

**Why deferred.** Scope. It is the single largest item in the aptui feature set
and it is orthogonal to everything else in Phase 1.

**Design sketch.** Field-aware queries: `section:editors`, `arch:amd64`,
`repo:noble/main`, `size>10MB`, `installed`, `!upgradable`, `order:size:desc`,
combined with free-text fuzzy search, AND-ed together.

```rust
// src/ui/filter.rs — grows a parse → predicate layer above the existing matcher

pub enum Term {
    Field { key: FieldKey, value: String },   // substring, case-insensitive
    Arch(String),                             // exact, case-insensitive
    Size { op: CmpOp, kb: u64 },
    Flag { key: FlagKey, negated: bool },
    Order { col: SortColumn, desc: bool },
    Free(String),
}
pub struct Query { pub terms: Vec<Term>, pub free_text: String }
pub fn parse(input: &str) -> Query;                       // pure, unit-testable
pub fn matches(q: &Query, row: &PackageRow, data: &AptData) -> bool;
```

**The signature problem to solve first.** `Filter::apply` (`src/ui/filter.rs:23`)
takes `&[String]` — labels only. Field-aware matching needs `PackageRow` and
`AptData`, so `refresh_package_filter` (`src/ui/app.rs:200`) is the single
integration point and the one signature that has to change.

`Size` and `Section` live in the `Packages` stanza, so they need the same
harvest-at-index-time treatment `Essential` and `Phased-Update-Percentage` got in
Phase 1 — **copy that implementation** (`memchr::memmem::find_iter` for the field
prefix + `partition_point` over stanza starts to find the owning stanza, in
`apt/lists.rs` `index_stanzas`). Do not parse these lazily per row; that is the
per-keystroke path.

**Files to touch.** `src/ui/filter.rs` (parser + predicate), `src/ui/app.rs:200`
(`refresh_package_filter`), `src/apt/lists.rs` (`PackageStub` gains `section`,
`installed_size_kb`), `src/apt/mod.rs` (`PackageRow` carries them forward).

**Dependencies required.** None. `nucleo-matcher` stays for the free-text part.

**Pitfalls observed in `aptui`.**
- **Two different tokenizers** (`internal/filter/filter.go` strips quotes,
  `internal/app/keypress_repo_filter.go` keeps them) that must agree and do not.
  Write one tokenizer with a `preserve_quotes: bool` flag.
- Version comparison in sorting is a plain string compare, so `1.10` sorts before
  `1.9`. This repo already has correct `apt::version::compare` — use it.
- The size filter formats the size into a display string and then **re-parses it
  back to a number** to compare. Keep a numeric `u64` alongside the display
  string.
- A package whose size failed to parse silently fails *any* size filter, so
  metadata gaps look like non-matches.
- Fuzzy matching is byte-indexed over lowercased ASCII — not UTF-8 safe. Use
  char indices.
- The minimum-quality threshold is computed from the raw pattern length
  *including spaces*, making multi-word queries disproportionately strict.

**Acceptance.** Table-driven parser tests for every term type, quoting, unknown
keys degrading to free text, and `size` with each comparison operator and unit.
Predicate tests against synthetic `PackageRow`s. A test asserting `1.9` sorts
before `1.10` under `order:version`.

---

## 3. Mirror fetch / test / apply

**Why deferred.** Needs HTTP, and the dependency decision was deferred with it.

**Design sketch.**

```rust
// src/mirrors/mod.rs  (new)
pub struct Distro { pub id: String, pub codename: String, pub pretty: String }
pub fn detect_distro() -> Result<Distro>;      // /etc/os-release, lsb_release -cs fallback
pub fn base_family(d: &Distro) -> Family;      // Ubuntu | Debian, via ID then ID_LIKE

pub fn fetch_mirror_list(f: Family) -> Result<Vec<String>>;
pub fn sample(mirrors: Vec<String>, max: usize) -> Vec<String>;   // even stride

pub struct Probe { pub url: String, pub latency: Option<Duration> }
/// Parallel via rayon (already a dependency). Hard per-probe timeout.
pub fn probe_all(mirrors: &[String], timeout: Duration) -> Vec<Probe>;

/// Rewrites config. MUST disable existing entries and take a backup.
pub fn plan_apply(winners: &[String], d: &Distro, sources: &Sources) -> ApplyPlan;
```

**Dependencies required.** Either extend the existing shell-out pattern —
`health/fixes.rs:173` `curl_get` already shells to `curl`, and
`curl -o /dev/null -s -w '%{time_total}' --max-time 3 -I <url>/dists/` gives
latency directly — or take `ureq` (blocking, rustls). The `curl` route adds no
crates and `curl` is present on every Debian/Ubuntu system; the `ureq` route adds
~2–3 MB to the binary but gives real timeout control and drops the runtime
dependency on `curl`. Decide when this is picked up; the `curl` route is
consistent with how the project already does networking.

**Pitfalls observed in `aptui`.** These are severe and are the main reason to
write this carefully rather than port it:
- **It writes a brand-new `/etc/apt/sources.list.d/aptui-mirrors.list` and
  neither disables the existing entries nor takes a backup.** The result is
  duplicated suites across two files. The correct implementation disables the
  superseded entries via `repos::rewrite_span` (`src/ui/repos.rs:368`) and backs
  up through `edit::backup` (`src/actions/edit.rs:26`) — both already exist and
  are already the mandatory path for every config write in this codebase.
- It emits one-line `deb` format only, never deb822 — even when the system is
  deb822 throughout. This project already has `repos::sources_to_deb822`
  (`src/ui/repos.rs:537`); match the format the user already uses.
- The mirror lists are scraped with a regex over raw HTML/RSS, with **no HTTP
  timeout** on those two fetches, so a hung server hangs the operation.
- Its own docs claim it runs `apt-get update` afterwards. It does not.

**Acceptance.** Pure-function tests for `base_family` (including the `ID_LIKE`
fallback), `sample` stride selection, and the generated source text for both
families. An apply test against a fixture tree asserting the old entries were
disabled and a backup exists.

---

## 4. Export / import package lists

**Why deferred.** Bundled with the persistence decision, though see below — a
useful subset needs no dependency at all.

**Design sketch.**

```rust
pub enum ExportSet { AllInstalled, ManualOnly }
pub fn export(data: &AptData, set: ExportSet, to: &Path) -> Result<usize>;
pub fn import(from: &Path) -> Result<Vec<String>>;
/// Names already installed are dropped; unknown names are reported, not passed
/// blindly to apt.
pub fn reconcile(names: Vec<String>, data: &AptData) -> Reconciled;
pub struct Reconciled { pub to_install: Vec<String>,
                        pub already: Vec<String>, pub unknown: Vec<String> }
```

**Cheap win available now.** The manual/auto split is already known
(`DpkgStatus::is_auto`, `apt/dpkg.rs:38`) and `edit::write_atomic`
(`actions/edit.rs:49`) is written and unused. A plain newline-delimited format —
the same shape `apt-mark showmanual` emits — needs **no dependency at all** and
covers the actual use case (rebuilding a machine). JSON only buys a place to put
versions and repo origins later. Consider shipping the text format ahead of the
rest of Phase 2.

**Files to touch.** New `src/portable.rs`; `PendingPrompt` (`src/ui/app.rs:121`)
gains path-input variants; the existing `Prompt` overlay (`src/ui/overlay.rs:19`)
is the input widget.

**Dependencies required.** None for the text format; `serde_json` if the JSON
schema is wanted.

**Pitfalls observed in `aptui`.**
- Its schema is `{"packages":[{"name":"curl"}]}` — names only, no versions and no
  repo origins, so an import cannot reproduce the source machine.
- The default export path resolves via a sudo-aware "real user home" helper, but
  an explicitly typed `~/foo.json` is expanded with the plain home lookup — so
  under `sudo` the two land in **different** home directories.
- Unknown package names are passed straight to apt, which then errors on the
  whole batch. Reconcile first (hence `Reconciled::unknown`).
- No atomic write, no fsync: a crash mid-write truncates the file.

**Acceptance.** Round-trip test; import against a fixture `AptData` asserting the
three-way `Reconciled` split; a test that `~` expansion resolves consistently
regardless of how the path was supplied.

---

## 5. Favorites

**Why deferred.** Needs persistence (same blocker as #1).

**Design sketch.** A persisted `Vec<String>` of package names; a `F` toggle;
favorites float to the top of the list, marked `★`.

**Naming.** Call it **favorites**, not "pins". APT's own *pin priority* is a
different concept that this app already surfaces for real, via
`apt::policy::policy_for` (`src/apt/policy.rs:28`) in the `v` version picker.
Reusing the word would make the version picker's "priority" column ambiguous.

**Files to touch.** New `src/favorites.rs`; the sort tail of
`refresh_package_filter` (`src/ui/app.rs:200`); the row renderer in
`src/ui/packages.rs`.

**Dependencies required.** `serde_json`, or a one-name-per-line text file — the
data is a list of strings, so this genuinely does not need a schema.

**Pitfalls observed in `aptui`.** The float-to-top only applies when no sort
column and no free-text query are active, which is reasonable but must be stated
in the UI or it reads as a bug. Decide the interaction with `SortState`
explicitly and document it in the sort module.

**Acceptance.** Toggle persists across a reload; favorites float only under the
documented conditions; the marker does not overflow the name column.

---

## 6. Background silent `apt-get update`

**Why deferred.** It changes system state without a visible confirmation, so it
needs a deliberate default and a settings surface to turn it off — and the app
has no settings file yet (same persistence blocker).

**Design sketch.** A worker thread running `apt-get update -qq` once after the
first successful load, posting its result over an `mpsc::Sender`.

**No async runtime is required.** The event loop already blocks on
`event::poll(Duration::from_millis(250))` (`src/ui/app.rs:295`), which is exactly
the tick needed to drain a results channel. Do not add `tokio`.

Two behaviours worth copying from aptui:
- It no-ops silently when there is no cached sudo ticket (`sudo -n`), which is
  the right failure mode for something the user did not ask for.
- It holds back its status message for ~2 seconds so a background refresh cannot
  stomp a success/error message the user is still reading. `set_status`
  (`src/ui/app.rs:182`) would need the same guard.

**Recommended default: off.** Turn it on from settings, not the other way round.

**Dependencies required.** None (`std::thread` + `std::sync::mpsc`).

**Acceptance.** With no sudo ticket, the app behaves identically to today. With
one, the upgradable set refreshes without a redraw stall, and a status message
set immediately before the refresh lands is not overwritten.

---

## 7. Log viewer

**Why deferred.** The user explicitly rejected an error-log *tab*. This entry is
about something narrower and is recorded as an open question, not a plan.

**The problem it addresses.** `App.log` (`src/ui/app.rs:145`) is written by
`set_status` (`:182`) and `log_command` (`:486`) and is **rendered nowhere**. The
doc comment on `log_command` currently reads:

> Record a privileged command's invocation and outcome in the log pane, so
> nothing that touches the system happens invisibly — the safety guarantee this
> app makes for every write.

There is no log pane, so as written the comment describes a guarantee the UI does
not deliver. Phase 1 corrects the comment.

**The option, if it is ever wanted.** An `L` overlay — not a tab — reusing the
shape of `overlay::render_help` (`src/ui/overlay.rs:98`), roughly 20 lines. It
would make the original comment true again. Ask before building it.

---

## 8. Phase 1 residue

Small things Phase 1 shipped as-is or deliberately left alone. Phase 1 is
now complete (tab shell, multi-select, hold/essential/phased, column
sorting, theme, mouse, Maintenance tasks) and verified end-to-end against a
real terminal (tmux + raw PTY) as well as `cargo test`/`cargo clippy`.

- **`apt::model::Origin` (`src/apt/model.rs:174`)**, **`apt::lists::stanza_map`
  (`src/apt/lists.rs:265`)**, **`DryRunPlan::raw` (`src/actions/apt.rs`)**,
  **`DpkgStatus::is_installed`**, **`InstalledPackage::architecture`/`source`**,
  **`RepoKey::host`**, **`SourceEntry::is_source_only`**,
  **`RepoUnit::entry_id`**, **`keyrings::orphaned_per_repo`**,
  **`edit::write_atomic`/`write_atomic_bytes`** — all pre-date Phase 1 and were
  already unused before this pass (verified by diffing warning output against
  the pre-Phase-1 commit); left untouched rather than doing an unrelated
  cleanup pass. `stanza_map` becomes useful once the query language (#2) needs
  field lookup by key; `write_atomic`/`write_atomic_bytes` become useful once
  export/import (#4) or the transaction journal (#1) need to write a plain
  file.
- **`Theme::fg`/`sel_fg`/`sel_bg`/`title` (`src/ui/theme.rs`)** are unused —
  reserved for a custom (non-`Modifier::REVERSED`) selection highlight color,
  which Phase 1's column-sort work (Step 7) didn't end up needing. Genuinely
  optional polish, not a gap.
- **`PackageScope::Installed`** was kept as a fourth scope-ring value (cycled
  with `s`) rather than folded into a separate "installed-only" toggle — that
  merge was in an earlier draft of the plan but wasn't in what the user
  actually approved. No conflict exists today; if a shared installed-only
  toggle is added later across tabs, revisit whether it should subsume this
  ring value (see the original plan draft's flag #3 for the reasoning if so).
- **`README.md`'s cold-start numbers are now measured, not estimated**: ~250 ms
  to load and index (65 repos / ~178k stanzas), ~430–500 ms including building
  the "All packages" row set the Packages tab needs at startup (varies with
  system load; `tests/perf_gate.rs` budgets 1500 ms / 2500 ms respectively).
  Re-measure if the reference machine changes.
- **Mouse support covers the Packages tab (both as its own tab and as the
  Repos-tab drill-down), the tab bar, the Repos tab's repository list, and
  both tabs' diagnostics/task lists** — row click, checkbox click, scroll,
  and column-header click-to-sort. It does not cover clicking inside modal
  overlays (confirm/prompt/help/version-picker), which must still be driven
  by keyboard.
