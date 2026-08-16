//! Column sorting for the package table.

use std::cmp::Ordering;

use crate::apt::model::InstallState;
use crate::apt::{version, PackageRow};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortColumn {
    Name,
    Version,
    State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SortState {
    /// `None` means "natural order" — name-ascending, the same order
    /// `AptData::all_packages`/`packages_in_repo` already produce.
    pub active: Option<(SortColumn, SortDir)>,
}

impl SortState {
    /// Click-to-sort tri-state: clicking a column not currently active
    /// starts it at ascending; clicking the active column toggles
    /// ascending → descending → (a third click) back to natural order.
    pub fn cycle_column(&mut self, col: SortColumn) {
        self.active = match self.active {
            Some((c, SortDir::Asc)) if c == col => Some((c, SortDir::Desc)),
            Some((c, SortDir::Desc)) if c == col => None,
            _ => Some((col, SortDir::Asc)),
        };
    }

    /// The `S` keyboard shortcut's ring, since not every column has a
    /// mouse-reachable header in every terminal: Name↑ → Name↓ → Version↑ →
    /// Version↓ → State↑ → State↓ → natural order → Name↑ …
    pub fn cycle_ring(&mut self) {
        use SortColumn::*;
        use SortDir::*;
        self.active = match self.active {
            None => Some((Name, Asc)),
            Some((Name, Asc)) => Some((Name, Desc)),
            Some((Name, Desc)) => Some((Version, Asc)),
            Some((Version, Asc)) => Some((Version, Desc)),
            Some((Version, Desc)) => Some((State, Asc)),
            Some((State, Asc)) => Some((State, Desc)),
            Some((State, Desc)) => None,
        };
    }

    /// `" ▲"`, `" ▼"`, or `""` — appended to a column header.
    pub fn indicator(&self, col: SortColumn) -> &'static str {
        match self.active {
            Some((c, SortDir::Asc)) if c == col => " \u{25b2}",
            Some((c, SortDir::Desc)) if c == col => " \u{25bc}",
            _ => "",
        }
    }
}

/// Installed-ness ordering for the `State` column: present states first,
/// broken next (it needs attention), then not-installed.
fn state_rank(s: InstallState) -> u8 {
    match s {
        InstallState::Installed => 0,
        InstallState::ConfigFiles => 1,
        InstallState::Broken => 2,
        InstallState::NotInstalled => 3,
    }
}

fn by_name(a: &PackageRow, b: &PackageRow) -> Ordering {
    a.name.to_lowercase().cmp(&b.name.to_lowercase())
}

/// Compare two rows for display order under `s`. `Version` uses real
/// Debian version ordering (`apt::version::compare`), not a string compare
/// — a plain string compare would rank `1.10` before `1.9`, which is
/// exactly the bug this exists to avoid (observed in the sibling Go project
/// this design was informed by, which sorts versions as plain strings).
/// A missing version sorts last regardless of direction — reversing a
/// "goes last" placement would put it first instead, which is why that
/// case is decided before `dir` is applied. `Name` is always the
/// tie-break, so equal keys don't reorder unpredictably between frames.
pub fn compare_rows(a: &PackageRow, b: &PackageRow, s: SortState) -> Ordering {
    let Some((col, dir)) = s.active else {
        return by_name(a, b);
    };

    if col == SortColumn::Version {
        let (a_empty, b_empty) = (a.version.is_empty(), b.version.is_empty());
        if a_empty || b_empty {
            return match (a_empty, b_empty) {
                (true, true) => by_name(a, b),
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => unreachable!(),
            };
        }
    }

    let ordering = match col {
        SortColumn::Name => by_name(a, b),
        SortColumn::Version => version::compare(&a.version, &b.version),
        SortColumn::State => state_rank(a.install_state).cmp(&state_rank(b.install_state)),
    };
    let primary = if dir == SortDir::Desc {
        ordering.reverse()
    } else {
        ordering
    };
    if primary == Ordering::Equal {
        by_name(a, b)
    } else {
        primary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apt::lists::StanzaRef;

    fn row(name: &str, version: &str, state: InstallState) -> PackageRow {
        PackageRow {
            name: name.to_string(),
            version: version.to_string(),
            install_state: state,
            auto_installed: false,
            upgradable: false,
            held: false,
            essential: false,
            phased_pct: None,
            stub: StanzaRef {
                file_id: usize::MAX,
                start: 0,
                end: 0,
            },
        }
    }

    #[test]
    fn cycle_column_tri_state() {
        let mut s = SortState::default();
        s.cycle_column(SortColumn::Name);
        assert_eq!(s.active, Some((SortColumn::Name, SortDir::Asc)));
        s.cycle_column(SortColumn::Name);
        assert_eq!(s.active, Some((SortColumn::Name, SortDir::Desc)));
        s.cycle_column(SortColumn::Name);
        assert_eq!(s.active, None);
    }

    #[test]
    fn cycle_column_switching_columns_restarts_at_asc() {
        let mut s = SortState {
            active: Some((SortColumn::Name, SortDir::Desc)),
        };
        s.cycle_column(SortColumn::Version);
        assert_eq!(s.active, Some((SortColumn::Version, SortDir::Asc)));
    }

    #[test]
    fn cycle_ring_visits_seven_states_and_wraps() {
        let mut s = SortState::default();
        let mut seen = vec![s.active];
        for _ in 0..7 {
            s.cycle_ring();
            seen.push(s.active);
        }
        assert_eq!(seen[0], None);
        assert_eq!(seen[7], None, "ring must return to natural order");
        // All seven non-initial states are distinct.
        let mid: std::collections::HashSet<_> = seen[1..7].iter().collect();
        assert_eq!(mid.len(), 6);
    }

    #[test]
    fn version_sort_uses_debian_ordering_not_string_ordering() {
        let a = row("pkg", "1.9", InstallState::Installed);
        let b = row("pkg", "1.10", InstallState::Installed);
        let s = SortState {
            active: Some((SortColumn::Version, SortDir::Asc)),
        };
        assert_eq!(
            compare_rows(&a, &b, s),
            Ordering::Less,
            "1.9 must sort before 1.10 under real version ordering"
        );
    }

    #[test]
    fn empty_version_sorts_last_in_both_directions() {
        let with_version = row("a", "1.0", InstallState::Installed);
        let without = row("b", "", InstallState::Installed);

        let asc = SortState {
            active: Some((SortColumn::Version, SortDir::Asc)),
        };
        assert_eq!(compare_rows(&with_version, &without, asc), Ordering::Less);
        assert_eq!(compare_rows(&without, &with_version, asc), Ordering::Greater);

        let desc = SortState {
            active: Some((SortColumn::Version, SortDir::Desc)),
        };
        assert_eq!(compare_rows(&with_version, &without, desc), Ordering::Less);
        assert_eq!(compare_rows(&without, &with_version, desc), Ordering::Greater);
    }

    #[test]
    fn state_column_orders_installed_before_not_installed() {
        let installed = row("a", "1.0", InstallState::Installed);
        let not_installed = row("b", "1.0", InstallState::NotInstalled);
        let s = SortState {
            active: Some((SortColumn::State, SortDir::Asc)),
        };
        assert_eq!(
            compare_rows(&installed, &not_installed, s),
            Ordering::Less
        );
    }

    #[test]
    fn name_is_the_tie_break_for_equal_keys() {
        let a = row("aaa", "1.0", InstallState::Installed);
        let b = row("bbb", "1.0", InstallState::Installed);
        let s = SortState {
            active: Some((SortColumn::Version, SortDir::Asc)),
        };
        assert_eq!(compare_rows(&a, &b, s), Ordering::Less);
        // Descending version with equal versions: tie-break stays
        // name-ascending, not reversed along with the primary key.
        let s_desc = SortState {
            active: Some((SortColumn::Version, SortDir::Desc)),
        };
        assert_eq!(compare_rows(&a, &b, s_desc), Ordering::Less);
    }

    #[test]
    fn indicator_shows_arrow_only_for_the_active_column_and_direction() {
        let s = SortState {
            active: Some((SortColumn::Name, SortDir::Asc)),
        };
        assert_eq!(s.indicator(SortColumn::Name), " \u{25b2}");
        assert_eq!(s.indicator(SortColumn::Version), "");
        assert_eq!(SortState::default().indicator(SortColumn::Name), "");
    }
}
