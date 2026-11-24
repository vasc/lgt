//! The TUI times its own work and, when it stops feeling instantaneous,
//! says so — and, when the cause is the amount of history, suggests how
//! much to archive to get back under budget.

use std::time::Duration;

use crate::store::Shelf;

/// Loading the shelf at startup should feel instant (~100ms is the classic
/// threshold for "instantaneous").
pub const LOAD_BUDGET: Duration = Duration::from_millis(100);
/// Work that can sit between a keypress and its redraw (a catalog refresh,
/// drawing a frame) gets a tighter budget.
pub const STEADY_BUDGET: Duration = Duration::from_millis(50);
/// A moving average needs this many samples before it may raise a warning,
/// so one slow first frame or refresh cannot do it alone.
pub const MIN_SAMPLES: u32 = 5;

/// Exponentially weighted moving average of a steady-state timing, in
/// microseconds, with an alarm against `STEADY_BUDGET`. The alarm raises
/// when the average *and* the latest sample are over budget (slow on
/// average and slow now, so a past hiccup alone cannot raise it) and clears
/// only once the average drops below 80% of the budget, so timings hovering
/// around it do not make the warning flicker.
#[derive(Debug, Default, Clone, Copy)]
pub struct Ewma {
    micros: Option<u64>,
    samples: u32,
    alarm: bool,
}

impl Ewma {
    pub fn record(&mut self, sample: Duration) {
        let sample = u64::try_from(sample.as_micros()).unwrap_or(u64::MAX);
        self.micros = Some(match self.micros {
            None => sample,
            Some(avg) => {
                avg.saturating_mul(7)
                    .saturating_add(sample.saturating_mul(3))
                    / 10
            }
        });
        self.samples = self.samples.saturating_add(1);
        let Some(average) = self.get() else {
            return;
        };
        let last = Duration::from_micros(sample);
        if !self.alarm && average > STEADY_BUDGET && last > STEADY_BUDGET {
            self.alarm = true;
        } else if self.alarm && average < STEADY_BUDGET * 4 / 5 {
            self.alarm = false;
        }
    }

    /// The average, once there are enough samples to trust it.
    pub fn get(&self) -> Option<Duration> {
        if self.samples < MIN_SAMPLES {
            return None;
        }
        self.micros.map(Duration::from_micros)
    }

    /// The average while the alarm is raised.
    pub fn alarm(&self) -> Option<Duration> {
        if self.alarm { self.get() } else { None }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Timings {
    /// Initial load (catalog plus row tree): elapsed time and how many
    /// streams it loaded.
    pub load: Option<(Duration, usize)>,
    /// Each periodic catalog refresh plus row-tree rebuild.
    pub refresh: Ewma,
    pub frame: Ewma,
}

impl Timings {
    pub fn record_load(&mut self, elapsed: Duration, streams: usize) {
        self.load = Some((elapsed, streams));
    }
}

/// One measured operation that blew its budget.
struct Overrun {
    what: &'static str,
    took: Duration,
    budget: Duration,
    /// Whether the cost grows with the number of streams on the shelf.
    /// Drawing is windowed, so its cost is the terminal's, not history's.
    scales_with_history: bool,
}

fn overruns(timings: &Timings, streams_now: usize) -> Vec<Overrun> {
    let mut out = Vec::new();
    if let Some((took, loaded)) = timings.load
        && loaded > 0
    {
        // Project the load cost onto today's stream count, so archiving in
        // another terminal clears the warning without restarting.
        let projected = took.saturating_mul(u32::try_from(streams_now).unwrap_or(u32::MAX))
            / u32::try_from(loaded).unwrap_or(u32::MAX);
        if projected > LOAD_BUDGET {
            out.push(Overrun {
                what: "loading",
                took: projected,
                budget: LOAD_BUDGET,
                scales_with_history: true,
            });
        }
    }
    let steady = [
        ("refreshing", timings.refresh, true),
        ("drawing", timings.frame, false),
    ];
    for (what, ewma, scales_with_history) in steady {
        if let Some(took) = ewma.alarm() {
            out.push(Overrun {
                what,
                took,
                budget: STEADY_BUDGET,
                scales_with_history,
            });
        }
    }
    out
}

/// The warning to show at the top of the TUI, if any. `total` counts every
/// stream on the shelf; `finished` holds the last-activity time of each
/// finished one (the only ones archive/clean can act on), in any order.
pub fn advice(
    timings: &Timings,
    shelf: Shelf,
    total: usize,
    finished: &[i64],
    now_ms: i64,
) -> Option<String> {
    let overruns = overruns(timings, total);
    // Report the operation that is furthest over its budget.
    let worst = overruns.iter().max_by(|a, b| {
        let ra = a.took.as_secs_f64() / a.budget.as_secs_f64();
        let rb = b.took.as_secs_f64() / b.budget.as_secs_f64();
        ra.total_cmp(&rb)
    })?;
    let took = worst.took.as_millis();
    let budget = worst.budget.as_millis();
    if !worst.scales_with_history {
        return Some(format!(
            "⚠ slow redraws: {took}ms (budget {budget}ms) — the terminal or connection is \
             slow; history size is not the cause"
        ));
    }
    let head = format!(
        "⚠ slow: {} {total} streams took {took}ms (budget {budget}ms)",
        worst.what
    );

    // Scale the stream count down by how far over budget we are, with 20%
    // headroom, and find the age that leaves about that many streams.
    let keep = usize::try_from(
        u128::try_from(total).unwrap_or(u128::MAX) * budget * 4 / (took.max(1) * 5),
    )
    .unwrap_or(total);
    let need = total.saturating_sub(keep).min(finished.len());
    if need == 0 {
        return Some(head);
    }
    let mut oldest_first = finished.to_vec();
    oldest_first.sort_unstable();
    // The newest stream that has to go. Selection is strictly-older-than,
    // so the age must be strictly below this stream's age (hence the 1ms);
    // rounding down to two units keeps the overshoot to at most one unit of
    // the smaller kind, even when all history is clustered in a few days.
    let boundary = oldest_first[need - 1];
    let boundary_age = now_ms.saturating_sub(boundary).saturating_sub(1);
    let Some((age_ms, age)) = crate::units::compact_age(boundary_age) else {
        return Some(head);
    };
    let cutoff = now_ms.saturating_sub(age_ms);
    let moved = oldest_first.iter().filter(|at| **at < cutoff).count();
    Some(match shelf {
        Shelf::Active => format!("{head} — run `lgt archive {age}` to archive {moved}"),
        Shelf::Archive => format!("{head} — run `lgt clean --archived {age}` to delete {moved}"),
    })
}

#[cfg(test)]
#[path = "perf_tests.rs"]
mod tests;
