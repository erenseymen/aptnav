//! Debian package version comparison, per `deb-version(7)`.
//!
//! Used to pick a "candidate" version when the same package name appears in
//! several repositories and to detect upgradable packages. This is
//! self-contained rather than shelled out to `dpkg` because it runs once per
//! comparison during index build (potentially thousands of times), where
//! process-spawn overhead would dominate; `apt-cache policy` remains the
//! source of truth for pin-priority-aware candidate selection in the detail
//! pane and version picker.

use std::cmp::Ordering;

/// Compare two version strings as dpkg would: `[epoch:]upstream[-revision]`.
pub fn compare(a: &str, b: &str) -> Ordering {
    let (ea, ua, ra) = split(a);
    let (eb, ub, rb) = split(b);
    ea.cmp(&eb)
        .then_with(|| compare_part(ua, ub))
        .then_with(|| compare_part(ra, rb))
}

fn split(v: &str) -> (u64, &str, &str) {
    let (epoch, rest) = match v.split_once(':') {
        Some((e, r)) => (e.parse().unwrap_or(0), r),
        None => (0, v),
    };
    match rest.rfind('-') {
        Some(i) => (epoch, &rest[..i], &rest[i + 1..]),
        None => (epoch, rest, ""),
    }
}

/// Compare one upstream-version or revision component per the algorithm in
/// deb-version(7): alternating non-digit and digit runs, non-digit runs
/// compared lexically (with `~` sorting before everything, including the
/// empty string), digit runs compared numerically.
fn compare_part(a: &str, b: &str) -> Ordering {
    let mut a = a.chars().peekable();
    let mut b = b.chars().peekable();

    loop {
        // Compare a run of non-digits lexically, `~` lowest of all.
        let mut ca = Vec::new();
        while let Some(&c) = a.peek() {
            if c.is_ascii_digit() {
                break;
            }
            ca.push(c);
            a.next();
        }
        let mut cb = Vec::new();
        while let Some(&c) = b.peek() {
            if c.is_ascii_digit() {
                break;
            }
            cb.push(c);
            b.next();
        }
        match cmp_non_digit_runs(&ca, &cb) {
            Ordering::Equal => {}
            other => return other,
        }

        // Compare a run of digits numerically.
        let mut na = String::new();
        while let Some(&c) = a.peek() {
            if !c.is_ascii_digit() {
                break;
            }
            na.push(c);
            a.next();
        }
        let mut nb = String::new();
        while let Some(&c) = b.peek() {
            if !c.is_ascii_digit() {
                break;
            }
            nb.push(c);
            b.next();
        }
        let va: u64 = na.parse().unwrap_or(0);
        let vb: u64 = nb.parse().unwrap_or(0);
        match va.cmp(&vb) {
            Ordering::Equal => {}
            other => return other,
        }

        if a.peek().is_none() && b.peek().is_none() {
            return Ordering::Equal;
        }
    }
}

/// `~` sorts before the empty string, which sorts before everything else;
/// otherwise plain character-by-character ordering using ASCII value, which
/// matches dpkg's rule that letters sort before non-letters at the same
/// position (both are just byte comparisons once `~` is special-cased).
fn cmp_non_digit_runs(a: &[char], b: &[char]) -> Ordering {
    let rank = |c: char| -> i32 {
        if c == '~' {
            -1
        } else if c.is_ascii_alphabetic() {
            c as i32
        } else {
            c as i32 + 256 // non-alpha, non-~ sorts after letters
        }
    };
    let mut i = 0;
    loop {
        let ca = a.get(i).copied();
        let cb = b.get(i).copied();
        match (ca, cb) {
            (None, None) => return Ordering::Equal,
            (None, Some(c)) => {
                return if c == '~' {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (Some(c), None) => {
                return if c == '~' {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (Some(x), Some(y)) => {
                let o = rank(x).cmp(&rank(y));
                if o != Ordering::Equal {
                    return o;
                }
            }
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Ordering::*;

    #[test]
    fn equal_versions() {
        assert_eq!(compare("1.0-1", "1.0-1"), Equal);
    }

    #[test]
    fn simple_numeric_upstream() {
        assert_eq!(compare("1.2", "1.10"), Less);
        assert_eq!(compare("1.10", "1.2"), Greater);
    }

    #[test]
    fn epoch_dominates() {
        assert_eq!(compare("1:1.0", "2.0"), Greater);
        assert_eq!(compare("2:1.0", "1:9.0"), Greater);
    }

    #[test]
    fn revision_breaks_ties() {
        assert_eq!(compare("1.0-1", "1.0-2"), Less);
        assert_eq!(compare("1.0-2ubuntu1", "1.0-2"), Greater);
    }

    #[test]
    fn tilde_sorts_before_anything() {
        // Pre-release convention: 1.0~rc1 < 1.0
        assert_eq!(compare("1.0~rc1", "1.0"), Less);
        assert_eq!(compare("1.0~~", "1.0~"), Less);
        assert_eq!(compare("1.0~", "1.0"), Less);
    }

    #[test]
    fn letters_sort_before_other_non_digits() {
        // From deb-version(7) worked examples.
        assert_eq!(compare("1.0", "1.0a"), Less);
    }

    #[test]
    fn real_world_ubuntu_versions() {
        assert_eq!(compare("8.5.0-2ubuntu10.6", "8.5.0-2ubuntu10"), Greater);
        assert_eq!(compare("3.12.3-0ubuntu2.1", "3.12.3-0ubuntu2"), Greater);
        assert_eq!(compare("152.0.7977.30-1", "153.0.7998.0-1"), Less);
    }

    #[test]
    fn no_revision_defaults_to_empty() {
        assert_eq!(compare("1.0", "1.0-1"), Less); // "" < "1" as a revision
    }
}
