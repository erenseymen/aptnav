//! Fuzzy filtering shared by the repository and package lists, backed by
//! `nucleo-matcher` (the matching engine from the Helix editor) so filtering
//! stays instant even over the "All packages" view's ~90k rows.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

pub struct Filter {
    matcher: Matcher,
    buf: Vec<char>,
}

impl Filter {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            buf: Vec::new(),
        }
    }

    /// Return indices into `items` that match `pattern`, sorted best-match
    /// first. An empty pattern matches everything in its original order.
    pub fn apply(&mut self, items: &[String], pattern: &str) -> Vec<usize> {
        if pattern.is_empty() {
            return (0..items.len()).collect();
        }
        let needle = Pattern::parse(pattern, CaseMatching::Ignore, Normalization::Smart);
        let mut scored: Vec<(usize, u32)> = Vec::new();
        for (i, s) in items.iter().enumerate() {
            let haystack = Utf32Str::new(s, &mut self.buf);
            if let Some(score) = needle.score(haystack, &mut self.matcher) {
                scored.push((i, score));
            }
        }
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored.into_iter().map(|(i, _)| i).collect()
    }
}

impl Default for Filter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pattern_returns_all_in_order() {
        let mut f = Filter::new();
        let items = vec!["b".to_string(), "a".to_string()];
        assert_eq!(f.apply(&items, ""), vec![0, 1]);
    }

    #[test]
    fn filters_and_ranks_matches() {
        let mut f = Filter::new();
        let items = vec![
            "firefox".to_string(),
            "python3".to_string(),
            "firefox-esr".to_string(),
        ];
        let result = f.apply(&items, "fire");
        assert!(result.contains(&0));
        assert!(result.contains(&2));
        assert!(!result.contains(&1));
    }
}
