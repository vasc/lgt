//! Which streams `lgt clean` / `lgt archive` act on.
//!
//! Every limit is independent: a stream is selected when it violates *any*
//! of them. All three limits select oldest-first, so combining them simply
//! applies the strictest one — each limit on its own is a guarantee.

use std::collections::HashSet;

#[derive(Debug, Default, Clone)]
pub struct Limits {
    /// Select streams whose last activity is more than this many ms ago.
    pub older_than_ms: Option<i64>,
    /// Keep only the newest N streams (running ones count towards N).
    pub keep: Option<usize>,
    /// Select oldest streams until the shelf's total size fits the cap.
    pub max_size: Option<u64>,
    /// Select every eligible stream. Must be asked for explicitly: no limit
    /// at all selects nothing.
    pub everything: bool,
}

impl Limits {
    pub fn is_empty(&self) -> bool {
        self.older_than_ms.is_none()
            && self.keep.is_none()
            && self.max_size.is_none()
            && !self.everything
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    /// Last activity (see `StreamMeta::last_activity_ms`); "oldest" and
    /// "newest" below always refer to this.
    pub at_ms: i64,
    pub size: u64,
    /// False for streams that must not be touched (still running).
    pub eligible: bool,
}

/// Ids to act on, oldest first.
pub fn select(candidates: &[Candidate], limits: &Limits, now_ms: i64) -> Vec<String> {
    let mut newest_first: Vec<&Candidate> = candidates.iter().collect();
    newest_first.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| b.id.cmp(&a.id)));

    let mut chosen: HashSet<&str> = HashSet::new();
    if limits.everything {
        chosen.extend(
            newest_first
                .iter()
                .filter(|c| c.eligible)
                .map(|c| c.id.as_str()),
        );
    }
    if let Some(age) = limits.older_than_ms {
        let cutoff = now_ms.saturating_sub(age);
        chosen.extend(
            newest_first
                .iter()
                .filter(|c| c.eligible && c.at_ms < cutoff)
                .map(|c| c.id.as_str()),
        );
    }
    if let Some(keep) = limits.keep {
        chosen.extend(
            newest_first
                .iter()
                .skip(keep)
                .filter(|c| c.eligible)
                .map(|c| c.id.as_str()),
        );
    }
    if let Some(cap) = limits.max_size {
        let mut total: u64 = newest_first
            .iter()
            .filter(|c| !chosen.contains(c.id.as_str()))
            .map(|c| c.size)
            .sum();
        for candidate in newest_first.iter().rev() {
            if total <= cap {
                break;
            }
            if candidate.eligible && chosen.insert(candidate.id.as_str()) {
                total = total.saturating_sub(candidate.size);
            }
        }
    }

    newest_first
        .iter()
        .rev()
        .filter(|c| chosen.contains(c.id.as_str()))
        .map(|c| c.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;
    const NOW: i64 = 1_000 * DAY;

    fn cand(id: &str, age_days: i64, size: u64, eligible: bool) -> Candidate {
        Candidate {
            id: id.to_string(),
            at_ms: NOW - age_days * DAY,
            size,
            eligible,
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_limits_select_nothing() {
        let c = [cand("a", 5, 1, true)];
        assert!(Limits::default().is_empty());
        assert!(select(&c, &Limits::default(), NOW).is_empty());
    }

    #[test]
    fn everything_selects_every_eligible_stream_oldest_first() {
        let c = [
            cand("b", 1, 1, true),
            cand("a", 5, 1, true),
            cand("live", 9, 1, false),
        ];
        let limits = Limits {
            everything: true,
            ..Limits::default()
        };
        assert_eq!(select(&c, &limits, NOW), ids(&["a", "b"]));
    }

    #[test]
    fn age_selects_strictly_older_streams() {
        let c = [
            cand("old", 31, 1, true),
            cand("edge", 30, 1, true),
            cand("new", 1, 1, true),
        ];
        let limits = Limits {
            older_than_ms: Some(30 * DAY),
            ..Limits::default()
        };
        assert_eq!(select(&c, &limits, NOW), ids(&["old"]));
    }

    #[test]
    fn keep_counts_running_streams_but_never_selects_them() {
        let c = [
            cand("live", 0, 1, false),
            cand("a", 1, 1, true),
            cand("b", 2, 1, true),
            cand("live-old", 3, 1, false),
        ];
        let limits = Limits {
            keep: Some(2),
            ..Limits::default()
        };
        assert_eq!(select(&c, &limits, NOW), ids(&["b"]));
    }

    #[test]
    fn max_size_drops_oldest_eligible_until_it_fits() {
        let c = [
            cand("n", 1, 10, true),
            cand("m", 2, 10, true),
            cand("live", 3, 10, false),
            cand("o", 4, 10, true),
        ];
        let limits = Limits {
            max_size: Some(15),
            ..Limits::default()
        };
        // 40 total; skipping the running stream, drop o (30), m (20), n (10).
        assert_eq!(select(&c, &limits, NOW), ids(&["o", "m", "n"]));
    }

    #[test]
    fn max_size_accounts_for_streams_other_limits_already_selected() {
        let c = [
            cand("n", 1, 10, true),
            cand("m", 2, 10, true),
            cand("o", 40, 10, true),
        ];
        let limits = Limits {
            older_than_ms: Some(30 * DAY),
            max_size: Some(20),
            ..Limits::default()
        };
        assert_eq!(select(&c, &limits, NOW), ids(&["o"]));
    }

    #[test]
    fn limits_are_a_union() {
        let c = [
            cand("a", 100, 1, true),
            cand("b", 50, 1, true),
            cand("c", 2, 1, true),
            cand("d", 1, 1, true),
        ];
        let limits = Limits {
            older_than_ms: Some(60 * DAY),
            keep: Some(2),
            ..Limits::default()
        };
        assert_eq!(select(&c, &limits, NOW), ids(&["a", "b"]));
    }
}
