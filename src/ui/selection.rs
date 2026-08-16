//! The set of packages selected for a bulk action.
//!
//! Keyed by package name, not by row index: unlike an index-based
//! selection, this cannot be corrupted by a filter change, a sort, or a
//! data reload reindexing `all_rows` out from under it — the structural fix
//! for a whole class of bug seen in `aptui` (the sibling Go project this
//! design was informed by), which keys its selection by name too but as a
//! `map[string]bool`. A Go map's iteration order is nondeterministic, so
//! the argv order it passes to `apt-get` isn't reproducible; `order` here
//! exists specifically to fix that — it's the only thing ever iterated for
//! building a command line, so packages are always acted on in the order
//! they were selected.

use std::collections::HashSet;

#[derive(Debug, Default, Clone)]
pub struct Selection {
    order: Vec<String>,
    index: HashSet<String>,
}

impl Selection {
    pub fn contains(&self, name: &str) -> bool {
        self.index.contains(name)
    }

    /// Returns `false` if `name` was already selected.
    pub fn insert(&mut self, name: &str) -> bool {
        if self.index.insert(name.to_string()) {
            self.order.push(name.to_string());
            true
        } else {
            false
        }
    }

    /// Returns `false` if `name` was not selected.
    pub fn remove(&mut self, name: &str) -> bool {
        if self.index.remove(name) {
            self.order.retain(|n| n != name);
            true
        } else {
            false
        }
    }

    /// Returns the new membership: `true` if `name` is selected afterward.
    pub fn toggle(&mut self, name: &str) -> bool {
        if self.contains(name) {
            self.remove(name);
            false
        } else {
            self.insert(name);
            true
        }
    }

    pub fn clear(&mut self) {
        self.order.clear();
        self.index.clear();
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Selection order, i.e. the order names will be passed to `apt-get` —
    /// never a `HashSet` iteration.
    pub fn names(&self) -> &[String] {
        &self.order
    }

    /// Select every name in `it` that isn't already selected. Returns how
    /// many were newly added.
    pub fn insert_all<'a>(&mut self, it: impl Iterator<Item = &'a str>) -> usize {
        it.filter(|n| self.insert(n)).count()
    }

    /// Deselect every name in `it` that is currently selected, leaving
    /// everything else untouched. This is what "deselect the filtered
    /// subset" must do — the aptui bug this fixes is a "deselect all" that
    /// instead nukes the *entire* selection, including members selected
    /// under a different filter.
    pub fn remove_all<'a>(&mut self, it: impl Iterator<Item = &'a str>) -> usize {
        it.filter(|n| self.remove(n)).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_round_trips() {
        let mut s = Selection::default();
        assert!(s.toggle("a"));
        assert!(s.contains("a"));
        assert!(!s.toggle("a"));
        assert!(!s.contains("a"));
    }

    #[test]
    fn insert_is_idempotent_and_preserves_order() {
        let mut s = Selection::default();
        assert!(s.insert("a"));
        assert!(s.insert("b"));
        assert!(!s.insert("a"), "already present");
        assert_eq!(s.names(), ["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn names_stay_insertion_ordered_after_an_interior_remove() {
        let mut s = Selection::default();
        s.insert("a");
        s.insert("b");
        s.insert("c");
        s.remove("b");
        assert_eq!(s.names(), ["a".to_string(), "c".to_string()]);
        s.insert("b");
        assert_eq!(
            s.names(),
            ["a".to_string(), "c".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn remove_all_leaves_non_matching_members_intact() {
        let mut s = Selection::default();
        s.insert_all(["a", "b", "c"].into_iter());
        let removed = s.remove_all(["a", "b"].into_iter());
        assert_eq!(removed, 2);
        assert_eq!(s.names(), ["c".to_string()]);
    }

    #[test]
    fn clear_empties_both_structures() {
        let mut s = Selection::default();
        s.insert_all(["a", "b"].into_iter());
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert!(!s.contains("a"));
        // Re-inserting after clear must work cleanly (the index was
        // actually cleared, not just `order`).
        assert!(s.insert("a"));
    }

    #[test]
    fn insert_all_reports_only_newly_added_count() {
        let mut s = Selection::default();
        s.insert("a");
        let added = s.insert_all(["a", "b", "c"].into_iter());
        assert_eq!(added, 2);
        assert_eq!(s.len(), 3);
    }
}
