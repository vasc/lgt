mod common;

use common::{
    DAY_MS, FakeState, archived_ids, fake_stream, now_ms, run, run_piped, stream_ids, temp_data_dir,
};

#[test]
fn archive_age_moves_old_finished_streams_off_the_active_shelf() {
    let dir = temp_data_dir();
    let now = now_ms();
    fake_stream(
        &dir,
        "s-old",
        "oldjob",
        now - 2 * DAY_MS,
        FakeState::Done,
        10,
    );
    fake_stream(
        &dir,
        "s-new",
        "newjob",
        now - 3_600_000,
        FakeState::Done,
        10,
    );
    fake_stream(
        &dir,
        "s-live",
        "livejob",
        now - 2 * DAY_MS,
        FakeState::Running,
        10,
    );

    let (ok, stdout, stderr) = run(&dir, &["archive", "24h"]);
    assert!(ok, "stderr: {stderr}");
    assert!(stdout.contains("archived 1 stream"), "stdout: {stdout}");
    assert_eq!(
        stream_ids(&dir),
        vec!["s-live", "s-new"],
        "running streams are never archived"
    );
    assert_eq!(archived_ids(&dir), vec!["s-old"]);

    // Default queries no longer see it ...
    let (_, listing, _) = run(&dir, &["ls"]);
    assert!(!listing.contains("oldjob"), "ls: {listing}");
    let (ok, _, _) = run(&dir, &["cat", "oldjob"]);
    assert!(!ok, "names resolve on the active shelf only");
    // ... but it can be reached deliberately.
    let (_, archived_listing, _) = run(&dir, &["ls", "--archived"]);
    assert!(archived_listing.contains("oldjob"), "{archived_listing}");
    assert!(!archived_listing.contains("newjob"), "{archived_listing}");
    let (ok, data, stderr) = run(&dir, &["cat", "--archived", "oldjob"]);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(data, "x".repeat(10));
    let (ok, data, stderr) = run(&dir, &["cat", "s-old"]);
    assert!(ok, "an exact id is found in the archive too: {stderr}");
    assert_eq!(data, "x".repeat(10));
}

#[test]
fn archive_age_counts_from_when_a_stream_last_wrote() {
    let dir = temp_data_dir();
    let now = now_ms();
    // A three-day build that finished an hour ago is not "older than 24h".
    fake_stream(
        &dir,
        "s-long",
        "long",
        now - 3 * DAY_MS,
        FakeState::Done,
        10,
    );
    let meta_path = dir.join("streams/s-long/meta.json");
    let mut meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
    meta["ended_at_ms"] = serde_json::json!(now - 3_600_000);
    std::fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();

    let (ok, _, stderr) = run(&dir, &["archive", "24h"]);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stream_ids(&dir), vec!["s-long"]);
    assert!(run(&dir, &["archive", "30m"]).0);
    assert_eq!(archived_ids(&dir), vec!["s-long"]);
}

#[test]
fn archive_without_a_limit_is_refused() {
    let dir = temp_data_dir();
    fake_stream(&dir, "s-1", "a", now_ms() - 2 * DAY_MS, FakeState::Done, 10);
    let (ok, _, stderr) = run(&dir, &["archive"]);
    assert!(!ok, "a bare `lgt archive` must not archive everything");
    assert!(
        stderr.contains("--everything"),
        "stderr must explain: {stderr}"
    );
    assert_eq!(stream_ids(&dir), vec!["s-1"]);
    assert!(run(&dir, &["archive", "--everything"]).0);
    assert_eq!(archived_ids(&dir), vec!["s-1"]);
}

#[test]
fn archive_dry_run_moves_nothing() {
    let dir = temp_data_dir();
    fake_stream(
        &dir,
        "s-old",
        "oldjob",
        now_ms() - 2 * DAY_MS,
        FakeState::Done,
        10,
    );
    let (ok, stdout, stderr) = run(&dir, &["archive", "24h", "--dry-run"]);
    assert!(ok, "stderr: {stderr}");
    assert!(
        stdout.contains("would archive 1 stream"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("s-old"), "stdout: {stdout}");
    assert_eq!(stream_ids(&dir), vec!["s-old"]);
    assert!(archived_ids(&dir).is_empty());
}

#[test]
fn unarchive_restores_named_streams() {
    let dir = temp_data_dir();
    fake_stream(
        &dir,
        "s-old",
        "oldjob",
        now_ms() - 2 * DAY_MS,
        FakeState::Done,
        10,
    );
    assert!(run(&dir, &["archive", "24h"]).0);
    let (ok, stdout, stderr) = run(&dir, &["unarchive", "oldjob", "s-old"]);
    assert!(ok, "the same stream named twice is restored once: {stderr}");
    assert!(stdout.contains("restored 1 stream"), "stdout: {stdout}");
    assert_eq!(stream_ids(&dir), vec!["s-old"]);
    assert!(archived_ids(&dir).is_empty());
    assert!(run(&dir, &["cat", "oldjob"]).0);
}

#[test]
fn unarchive_since_restores_recent_streams_in_bulk() {
    let dir = temp_data_dir();
    let now = now_ms();
    for (id, age_days) in [("s-a", 2), ("s-b", 3), ("s-c", 10), ("s-d", 20)] {
        fake_stream(&dir, id, id, now - age_days * DAY_MS, FakeState::Done, 10);
    }
    assert!(run(&dir, &["archive", "24h"]).0);
    assert_eq!(archived_ids(&dir).len(), 4);

    let (ok, stdout, stderr) = run(&dir, &["unarchive", "--since", "5d", "--dry-run"]);
    assert!(ok, "stderr: {stderr}");
    assert!(
        stdout.contains("would restore 2 stream"),
        "stdout: {stdout}"
    );
    assert_eq!(archived_ids(&dir).len(), 4, "dry run moves nothing");

    let (ok, _, stderr) = run(&dir, &["unarchive", "--since", "5d"]);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stream_ids(&dir), vec!["s-a", "s-b"]);
    assert_eq!(archived_ids(&dir), vec!["s-c", "s-d"]);
}

#[test]
fn unarchive_needs_references_or_since() {
    let dir = temp_data_dir();
    let (ok, _, stderr) = run(&dir, &["unarchive"]);
    assert!(!ok);
    assert!(stderr.contains("--since"), "stderr: {stderr}");
}

#[test]
fn archived_flag_is_rejected_where_it_would_be_ignored() {
    let dir = temp_data_dir();
    fake_stream(&dir, "s-1", "a", now_ms() - 2 * DAY_MS, FakeState::Done, 10);
    for args in [
        &["archive", "--archived", "1h"][..],
        &["unarchive", "--archived", "a"][..],
        &["path", "--archived"][..],
    ] {
        let (ok, _, stderr) = run(&dir, args);
        assert!(!ok, "{args:?} must be rejected");
        assert!(stderr.contains("--archived"), "{args:?}: {stderr}");
    }
    let (ok, _, _) = run_piped(&dir, &["--archived"], b"data\n");
    assert!(!ok, "captures always go to the active streams");
    assert_eq!(stream_ids(&dir), vec!["s-1"]);
}

#[test]
fn archive_include_running_moves_captures_silent_for_longer_than_age() {
    let dir = temp_data_dir();
    let now = now_ms();
    // A capture that stopped writing 40 days ago but still looks running
    // (e.g. recorded in a container instance that no longer exists).
    fake_stream(
        &dir,
        "s-stuck",
        "stuck",
        now - 40 * DAY_MS,
        FakeState::Running,
        10,
    );
    fake_stream(
        &dir,
        "s-busy",
        "busy",
        now - 3_600_000,
        FakeState::Running,
        10,
    );
    let (ok, _, _) = run(&dir, &["archive", "30d"]);
    assert!(ok);
    assert!(
        archived_ids(&dir).is_empty(),
        "running streams need the flag"
    );
    let (ok, _, stderr) = run(&dir, &["archive", "30d", "--include-running"]);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(archived_ids(&dir), vec!["s-stuck"]);
    assert_eq!(
        stream_ids(&dir),
        vec!["s-busy"],
        "a busy capture is not old"
    );
}
