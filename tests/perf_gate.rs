//! Performance gate: cold start to an interactive repo list must stay well
//! under the "feels instant" threshold. Benchmarked at ~150ms on a real
//! machine with 63 `Packages` files / 189MB / ~178k package stanzas — the
//! budget here is set generously above that (measured machines vary, and
//! CI runners are often slower/noisier than a dev laptop) while still
//! catching a real regression (e.g. an accidental O(n^2) creeping into the
//! indexer) rather than passing trivially.
//!
//! Skips (rather than fails) when run somewhere without a real APT
//! installation — e.g. a non-Debian CI image — since there's nothing to
//! measure there.

#[path = "../src/apt/mod.rs"]
mod apt;

use std::time::{Duration, Instant};

const BUDGET: Duration = Duration::from_millis(1500);
/// `all_packages()` additionally walks every distinct name's best stub and
/// builds a full `PackageRow` for it — this budget covers `load` plus that,
/// since the Packages tab makes `all_packages()` part of cold start too.
const BUDGET_WITH_ALL_PACKAGES: Duration = Duration::from_millis(2500);

#[test]
fn cold_start_is_fast_on_the_real_system() {
    let roots = apt::AptRoots::default();
    if !roots.lists_dir.is_dir() {
        eprintln!(
            "skipping: {} not present (not a Debian/Ubuntu system)",
            roots.lists_dir.display()
        );
        return;
    }

    let t0 = Instant::now();
    let data = match apt::AptData::load(roots, true) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: could not load real APT state: {e}");
            return;
        }
    };
    let elapsed = t0.elapsed();

    eprintln!(
        "loaded {} packages / {} repos / {} list files in {:?}",
        data.lists.stubs.len(),
        data.repos.len(),
        data.lists.files.len(),
        elapsed
    );

    assert!(
        elapsed < BUDGET,
        "cold start took {elapsed:?}, budget is {BUDGET:?} — investigate a regression \
         before raising this threshold"
    );
}

/// The Packages tab is the opening view, so `all_packages()` — previously
/// only paid when a user drilled into "All packages" from the repo list — now
/// runs unconditionally at startup. Measured separately from
/// `cold_start_is_fast_on_the_real_system` so a regression can be attributed
/// to `AptData::load` itself versus row-building on top of it.
#[test]
fn cold_start_plus_all_packages_is_fast() {
    let roots = apt::AptRoots::default();
    if !roots.lists_dir.is_dir() {
        eprintln!(
            "skipping: {} not present (not a Debian/Ubuntu system)",
            roots.lists_dir.display()
        );
        return;
    }

    let t0 = Instant::now();
    let data = match apt::AptData::load(roots, true) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: could not load real APT state: {e}");
            return;
        }
    };
    let rows = data.all_packages();
    let elapsed = t0.elapsed();

    eprintln!(
        "loaded + built {} package rows in {:?}",
        rows.len(),
        elapsed
    );

    assert!(
        elapsed < BUDGET_WITH_ALL_PACKAGES,
        "load + all_packages took {elapsed:?}, budget is {BUDGET_WITH_ALL_PACKAGES:?} — \
         investigate a regression before raising this threshold"
    );
}
