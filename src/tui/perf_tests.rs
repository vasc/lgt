use super::*;

const HOUR: i64 = 3_600_000;
const NOW: i64 = 10_000 * HOUR;

/// One finished stream per hour, newest first.
fn hourly(count: usize) -> Vec<i64> {
    (0..count)
        .map(|i| NOW - i64::try_from(i).unwrap() * HOUR)
        .collect()
}

fn slow_load(ms: u64, streams: usize) -> Timings {
    let mut t = Timings::default();
    t.record_load(Duration::from_millis(ms), streams);
    t
}

#[test]
fn fast_stores_get_no_warning() {
    let mut t = slow_load(20, 1000);
    for _ in 0..10 {
        t.refresh.record(Duration::from_millis(3));
        t.frame.record(Duration::from_millis(4));
    }
    assert_eq!(advice(&t, Shelf::Active, 1000, &hourly(1000), NOW), None);
}

#[test]
fn slow_load_suggests_an_archive_age_that_fits_the_budget() {
    // 1000 streams in 400ms: 4x over budget, so keep ~1000/4*0.8 = 200.
    let t = slow_load(400, 1000);
    let text = advice(&t, Shelf::Active, 1000, &hourly(1000), NOW).unwrap();
    assert!(text.contains("loading 1000 streams took 400ms"), "{text}");
    // Stream #200 ended exactly 200h ago; it must be included, so the age
    // rounds down from just under 200h to `8d7h`, which moves exactly the
    // 800 streams the budget calls for.
    assert!(
        text.contains("run `lgt archive 8d7h` to archive 800"),
        "{text}"
    );
}

#[test]
fn suggestion_does_not_overshoot_when_history_is_clustered() {
    // 5000 streams one minute apart, all about 56 days old: rounding the
    // boundary age down to whole days would archive every one of them.
    const MINUTE: i64 = 60_000;
    let finished: Vec<i64> = (0..5000)
        .map(|i| NOW - 56 * 24 * HOUR - i * MINUTE)
        .collect();
    let t = slow_load(578, 5000);
    let text = advice(&t, Shelf::Active, 5000, &finished, NOW).unwrap();
    // keep = 5000 * 100 * 4 / (578 * 5) = 692, so 4308 must move; the
    // two-unit age `56d11h` moves 4339 (an hour of extra streams at most).
    assert!(
        text.contains("`lgt archive 56d11h` to archive 4339"),
        "{text}"
    );
}

#[test]
fn running_streams_are_not_counted_as_archivable() {
    // 1000 streams, of which the newest 100 are running (not in `finished`).
    let finished: Vec<i64> = hourly(1000).into_iter().skip(100).collect();
    let t = slow_load(400, 1000);
    let text = advice(&t, Shelf::Active, 1000, &finished, NOW).unwrap();
    // Still 800 must go; the count only includes finished streams.
    assert!(text.contains("to archive 800"), "{text}");
}

#[test]
fn archiving_elsewhere_clears_the_load_warning() {
    let t = slow_load(400, 1000);
    assert!(advice(&t, Shelf::Active, 200, &hourly(200), NOW).is_none());
}

#[test]
fn steady_state_refresh_slowness_suggests_archiving() {
    let mut t = slow_load(10, 500);
    for _ in 0..10 {
        t.refresh.record(Duration::from_millis(200));
    }
    let text = advice(&t, Shelf::Active, 500, &hourly(500), NOW).unwrap();
    assert!(text.contains("refreshing 500 streams"), "{text}");
    assert!(text.contains("lgt archive"), "{text}");
}

#[test]
fn slow_drawing_is_blamed_on_the_terminal_not_history() {
    let mut t = slow_load(10, 500);
    for _ in 0..10 {
        t.frame.record(Duration::from_millis(200));
    }
    let text = advice(&t, Shelf::Active, 500, &hourly(500), NOW).unwrap();
    assert!(text.contains("slow redraws"), "{text}");
    assert!(
        !text.contains("lgt archive"),
        "archiving cannot help: {text}"
    );
}

#[test]
fn a_single_slow_sample_does_not_warn() {
    let mut t = slow_load(10, 500);
    t.frame.record(Duration::from_millis(500));
    t.refresh.record(Duration::from_millis(500));
    assert_eq!(advice(&t, Shelf::Active, 500, &hourly(500), NOW), None);
}

#[test]
fn archive_shelf_suggests_cleaning_instead() {
    let t = slow_load(400, 1000);
    let text = advice(&t, Shelf::Archive, 1000, &hourly(1000), NOW).unwrap();
    assert!(
        text.contains("`lgt clean --archived 8d7h` to delete 800"),
        "{text}"
    );
}

#[test]
fn no_suggestion_when_history_is_too_recent_to_split() {
    let t = slow_load(400, 1000);
    // Everything finished within the last second.
    let finished: Vec<i64> = (0..1000).map(|i| NOW - i).collect();
    let text = advice(&t, Shelf::Active, 1000, &finished, NOW).unwrap();
    assert!(!text.contains("lgt archive"), "{text}");
}

#[test]
fn ewma_smooths_single_spikes() {
    let mut e = Ewma::default();
    for _ in 0..10 {
        e.record(Duration::from_millis(10));
    }
    e.record(Duration::from_millis(100));
    assert!(e.get().unwrap() < Duration::from_millis(50));
}

#[test]
fn a_hiccup_followed_by_fast_samples_does_not_warn() {
    // 500ms then 5ms × 4: the average is still ~124ms after five samples.
    let mut t = slow_load(10, 500);
    t.frame.record(Duration::from_millis(500));
    t.refresh.record(Duration::from_millis(500));
    for _ in 0..4 {
        t.frame.record(Duration::from_millis(5));
        t.refresh.record(Duration::from_millis(5));
    }
    assert_eq!(advice(&t, Shelf::Active, 500, &hourly(500), NOW), None);
}

#[test]
fn alternating_timings_do_not_make_the_warning_flicker() {
    let mut t = slow_load(10, 500);
    let mut shown = Vec::new();
    for i in 0..40 {
        let ms = if i % 2 == 0 { 30 } else { 150 };
        t.refresh.record(Duration::from_millis(ms));
        shown.push(advice(&t, Shelf::Active, 500, &hourly(500), NOW).is_some());
    }
    let first_on = shown.iter().position(|s| *s).expect("eventually warns");
    assert!(
        shown[first_on..].iter().all(|s| *s),
        "once raised, the warning must not toggle: {shown:?}"
    );
    // It clears once things are clearly fast again.
    for _ in 0..30 {
        t.refresh.record(Duration::from_millis(5));
    }
    assert_eq!(advice(&t, Shelf::Active, 500, &hourly(500), NOW), None);
}
