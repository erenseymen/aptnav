//! Screen geometry recorded during rendering so mouse handling (Step 9) has
//! exactly one place to ask "what's under this point" — rather than each
//! mouse handler re-deriving column/row boundaries with its own arithmetic
//! that can drift out of sync with what was actually drawn. The renderer is
//! the only writer; `hit_test` only ever reads.

use ratatui::layout::Rect;

use super::app::Tab;
use super::sort::SortColumn;

#[derive(Debug, Default, Clone)]
pub struct LayoutCache {
    pub tab_hits: Vec<(Rect, Tab)>,
    /// The package table's sortable column headers (Name/Version/State) —
    /// shared by the Packages tab and the Repos-tab drill view, since both
    /// render through the same `packages::draw`.
    pub pkg_col_hits: Vec<(Rect, SortColumn)>,
    /// The package table's row area (borders already excluded) and the
    /// index of the first visible row, for translating a click's `y` into
    /// a row index and for wheel-scroll hit testing.
    pub pkg_rows: Rect,
    pub pkg_row_offset: usize,
    /// Width, from `pkg_rows.x`, of the `[x]`/`[ ]` checkbox glyph — a click
    /// inside this span toggles selection instead of just moving the
    /// cursor.
    pub pkg_checkbox_width: u16,
    /// The Repos tab's repository list (List mode only).
    pub repo_rows: Rect,
    pub repo_row_offset: usize,
    /// Whichever diagnostics list is currently on screen — the Repos tab's
    /// right pane in List mode, or the Maintenance tab's left pane. Only
    /// one is ever visible at a time, so one field covers both.
    pub diag_rows: Rect,
    pub diag_row_offset: usize,
    /// The Maintenance tab's task list.
    pub task_rows: Rect,
    pub task_row_offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Tab(Tab),
    SortHeader(SortColumn),
    PackageRow { index: usize, on_checkbox: bool },
    RepoRow { index: usize },
    DiagRow { index: usize },
    TaskRow { index: usize },
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

fn row_hit(rect: Rect, offset: usize, x: u16, y: u16) -> Option<usize> {
    if contains(rect, x, y) {
        Some(offset + (y - rect.y) as usize)
    } else {
        None
    }
}

/// What's under `(x, y)`, checked in the order a click should resolve:
/// tabs, then the package column headers, then whichever row area actually
/// contains the point. Purely a lookup into `cache` — never recomputes
/// geometry.
pub fn hit_test(cache: &LayoutCache, x: u16, y: u16) -> Option<Hit> {
    for &(rect, tab) in &cache.tab_hits {
        if contains(rect, x, y) {
            return Some(Hit::Tab(tab));
        }
    }
    for &(rect, col) in &cache.pkg_col_hits {
        if contains(rect, x, y) {
            return Some(Hit::SortHeader(col));
        }
    }
    if let Some(index) = row_hit(cache.pkg_rows, cache.pkg_row_offset, x, y) {
        let on_checkbox = x < cache.pkg_rows.x + cache.pkg_checkbox_width;
        return Some(Hit::PackageRow { index, on_checkbox });
    }
    if let Some(index) = row_hit(cache.repo_rows, cache.repo_row_offset, x, y) {
        return Some(Hit::RepoRow { index });
    }
    if let Some(index) = row_hit(cache.diag_rows, cache.diag_row_offset, x, y) {
        return Some(Hit::DiagRow { index });
    }
    if let Some(index) = row_hit(cache.task_rows, cache.task_row_offset, x, y) {
        return Some(Hit::TaskRow { index });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> LayoutCache {
        LayoutCache {
            tab_hits: vec![
                (Rect::new(0, 0, 10, 1), Tab::Packages),
                (Rect::new(10, 0, 10, 1), Tab::Repos),
            ],
            pkg_col_hits: vec![
                (Rect::new(0, 2, 20, 1), SortColumn::Name),
                (Rect::new(20, 2, 10, 1), SortColumn::Version),
            ],
            pkg_rows: Rect::new(0, 3, 30, 10),
            pkg_row_offset: 5,
            pkg_checkbox_width: 3,
            repo_rows: Rect::default(),
            repo_row_offset: 0,
            diag_rows: Rect::default(),
            diag_row_offset: 0,
            task_rows: Rect::default(),
            task_row_offset: 0,
        }
    }

    #[test]
    fn hit_test_resolves_tab_click() {
        assert_eq!(hit_test(&cache(), 3, 0), Some(Hit::Tab(Tab::Packages)));
        assert_eq!(hit_test(&cache(), 15, 0), Some(Hit::Tab(Tab::Repos)));
    }

    #[test]
    fn hit_test_resolves_sort_header_click() {
        assert_eq!(
            hit_test(&cache(), 5, 2),
            Some(Hit::SortHeader(SortColumn::Name))
        );
        assert_eq!(
            hit_test(&cache(), 25, 2),
            Some(Hit::SortHeader(SortColumn::Version))
        );
    }

    #[test]
    fn hit_test_resolves_row_click_respecting_offset() {
        // y=3 is the first row in view (row_in_view=0), which maps to the
        // absolute row index `pkg_row_offset + 0`.
        assert_eq!(
            hit_test(&cache(), 5, 3),
            Some(Hit::PackageRow {
                index: 5,
                on_checkbox: false
            })
        );
        assert_eq!(
            hit_test(&cache(), 5, 6),
            Some(Hit::PackageRow {
                index: 8,
                on_checkbox: false
            })
        );
    }

    #[test]
    fn hit_test_distinguishes_checkbox_from_the_rest_of_the_row() {
        assert_eq!(
            hit_test(&cache(), 0, 3),
            Some(Hit::PackageRow {
                index: 5,
                on_checkbox: true
            })
        );
        assert_eq!(
            hit_test(&cache(), 2, 3),
            Some(Hit::PackageRow {
                index: 5,
                on_checkbox: true
            })
        );
        assert_eq!(
            hit_test(&cache(), 3, 3),
            Some(Hit::PackageRow {
                index: 5,
                on_checkbox: false
            })
        );
    }

    #[test]
    fn hit_test_one_pixel_outside_every_rect_is_none() {
        assert_eq!(hit_test(&cache(), 30, 0), None); // past the tabs
        assert_eq!(hit_test(&cache(), 0, 1), None); // between tabs and headers
        assert_eq!(hit_test(&cache(), 0, 13), None); // just past the rows
    }

    #[test]
    fn hit_test_resolves_repo_diag_and_task_rows() {
        let mut c = cache();
        c.pkg_rows = Rect::default(); // out of the way, so it can't shadow the others
        c.repo_rows = Rect::new(0, 3, 20, 5);
        c.repo_row_offset = 2;
        c.diag_rows = Rect::new(20, 3, 20, 5);
        c.diag_row_offset = 1;
        c.task_rows = Rect::new(40, 3, 20, 5);
        c.task_row_offset = 0;

        assert_eq!(hit_test(&c, 0, 4), Some(Hit::RepoRow { index: 3 }));
        assert_eq!(hit_test(&c, 20, 4), Some(Hit::DiagRow { index: 2 }));
        assert_eq!(hit_test(&c, 40, 4), Some(Hit::TaskRow { index: 1 }));
    }
}
