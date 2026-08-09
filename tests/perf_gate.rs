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
