mod common;

use common::{
    DAY_MS, FakeState, archived_ids, fake_stream, now_ms, read_meta, run, spawn_and_kill,
    stream_ids, temp_data_dir,
};

fn clean(dir: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let mut full = vec!["clean"];
    full.extend_from_slice(args);
    run(dir, &full)
}

#[test]
fn clean_age_removes_only_old_finished_streams() {
    let dir = temp_data_dir();
    let now = now_ms();
    fake_stream(&dir, "s-old", "old", now - 40 * DAY_MS, FakeState::Done, 10);
    fake_stream(&dir, "s-new", "new", now - DAY_MS, FakeState::Done, 10);
    fake_stream(
        &dir,
        "s-live",
        "live",
        now - 40 * DAY_MS,
        FakeState::Running,
        10,
    );

    let (ok, stdout, stderr) = clean(&dir, &["30d"]);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(
        stream_ids(&dir),
        vec!["s-live", "s-new"],
        "only the old finished stream may go; running streams are never pruned:\n{stdout}"
    );
    assert!(stdout.contains("removed 1 stream"), "stdout: {stdout}");
}

#[test]
fn clean_without_a_limit_is_refused() {
    let dir = temp_data_dir();
    fake_stream(
        &dir,
        "s-1",
        "a",
        now_ms() - 40 * DAY_MS,
        FakeState::Done,
        10,
    );
    let (ok, _, stderr) = clean(&dir, &[]);
    assert!(!ok, "a bare `lgt clean` must not delete everything");
    assert!(
        stderr.contains("--everything"),
        "stderr must explain: {stderr}"
    );
    assert_eq!(stream_ids(&dir), vec!["s-1"]);
}

#[test]
fn clean_everything_removes_every_finished_stream() {
    let dir = temp_data_dir();
    let now = now_ms();
    fake_stream(&dir, "s-1", "a", now, FakeState::Done, 10);
    fake_stream(&dir, "s-2", "b", now - DAY_MS, FakeState::Done, 10);
    fake_stream(&dir, "s-live", "live", now, FakeState::Running, 10);
    let (ok, _, stderr) = clean(&dir, &["--everything"]);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stream_ids(&dir), vec!["s-live"]);
}

#[test]
fn clean_include_running_also_removes_running_streams() {
    let dir = temp_data_dir();
    fake_stream(
        &dir,
        "s-live",
        "live",
        now_ms() - 40 * DAY_MS,
        FakeState::Running,
        10,
    );
    let (ok, _, stderr) = clean(&dir, &["30d", "--include-running"]);
    assert!(ok, "stderr: {stderr}");
    assert!(stream_ids(&dir).is_empty());
}

#[test]
fn clean_keep_retains_the_newest_n() {
    let dir = temp_data_dir();
    let now = now_ms();
    for (i, id) in ["s-a", "s-b", "s-c", "s-d", "s-e"].iter().enumerate() {
        let age = i64::try_from(5 - i).unwrap() * DAY_MS;
        fake_stream(&dir, id, id, now - age, FakeState::Done, 10);
    }
    let (ok, _, stderr) = clean(&dir, &["--keep", "2"]);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stream_ids(&dir), vec!["s-d", "s-e"]);
}

#[test]
fn clean_max_size_trims_oldest_first_until_under_the_cap() {
    let dir = temp_data_dir();
    let now = now_ms();
    for (i, id) in ["s-1", "s-2", "s-3", "s-4"].iter().enumerate() {
        let age = i64::try_from(4 - i).unwrap() * DAY_MS;
        fake_stream(&dir, id, id, now - age, FakeState::Done, 10 * 1024);
    }
    // ~10.4KiB per stream (data + meta): 4 streams ≈ 42KiB. A 25KiB cap
    // requires dropping the two oldest.
    let (ok, stdout, stderr) = clean(&dir, &["--max-size", "25K"]);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stream_ids(&dir), vec!["s-3", "s-4"], "stdout: {stdout}");
}

#[test]
fn clean_limits_are_independent() {
    let dir = temp_data_dir();
    let now = now_ms();
    fake_stream(&dir, "s-1", "a", now - 100 * DAY_MS, FakeState::Done, 10);
    fake_stream(&dir, "s-2", "b", now - 50 * DAY_MS, FakeState::Done, 10);
    fake_stream(&dir, "s-3", "c", now - 2 * DAY_MS, FakeState::Done, 10);
    fake_stream(&dir, "s-4", "d", now - DAY_MS, FakeState::Done, 10);
    fake_stream(&dir, "s-5", "e", now, FakeState::Done, 10);
    // The age alone would remove only s-1; --keep 3 also removes s-2.
    let (ok, _, stderr) = clean(&dir, &["60d", "--keep", "3"]);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stream_ids(&dir), vec!["s-3", "s-4", "s-5"]);
}

#[test]
fn clean_dry_run_reports_without_removing() {
    let dir = temp_data_dir();
    let now = now_ms();
    fake_stream(
        &dir,
        "s-old",
        "oldjob",
        now - 40 * DAY_MS,
        FakeState::Done,
        10,
    );
    fake_stream(&dir, "s-new", "newjob", now, FakeState::Done, 10);
    let (ok, stdout, stderr) = clean(&dir, &["30d", "--dry-run"]);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(
        stream_ids(&dir).len(),
        2,
        "dry run must not remove anything"
    );
    assert!(stdout.contains("would remove 1 stream"), "stdout: {stdout}");
    assert!(
        stdout.contains("s-old"),
        "stdout must list the candidate: {stdout}"
    );
    assert!(!stdout.contains("s-new"), "stdout: {stdout}");
}

#[test]
fn clean_rejects_malformed_limits() {
    let dir = temp_data_dir();
    fake_stream(&dir, "s-1", "a", now_ms(), FakeState::Done, 10);
    for args in [
        &["30"][..],
        &["soon"][..],
        &["--max-size", "big"][..],
        &["--max-size", "10Q"][..],
    ] {
        let (ok, _, stderr) = clean(&dir, args);
        assert!(!ok, "{args:?} must be rejected");
        assert!(!stderr.is_empty(), "{args:?} must explain the error");
    }
    assert_eq!(
        stream_ids(&dir).len(),
        1,
        "a rejected clean removes nothing"
    );
}

#[test]
fn clean_archived_deletes_only_from_the_archive() {
    let dir = temp_data_dir();
    let now = now_ms();
    fake_stream(&dir, "s-a", "a", now - 10 * DAY_MS, FakeState::Done, 10);
    fake_stream(&dir, "s-b", "b", now - 10 * DAY_MS, FakeState::Done, 10);
    fake_stream(&dir, "s-c", "c", now - 100 * DAY_MS, FakeState::Done, 10);
    assert!(run(&dir, &["archive", "7d"]).0);
    fake_stream(&dir, "s-d", "d", now - 100 * DAY_MS, FakeState::Done, 10);

    let (ok, _, stderr) = clean(&dir, &["--archived", "30d"]);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(archived_ids(&dir), vec!["s-a", "s-b"]);
    assert_eq!(
        stream_ids(&dir),
        vec!["s-d"],
        "the active shelf is untouched by an archive clean"
    );
}

#[test]
fn rm_archived_removes_from_the_archive() {
    let dir = temp_data_dir();
    fake_stream(
        &dir,
        "s-a",
        "a",
        now_ms() - 10 * DAY_MS,
        FakeState::Done,
        10,
    );
    assert!(run(&dir, &["archive", "7d"]).0);
    let (ok, _, _) = run(&dir, &["rm", "a"]);
    assert!(!ok, "names resolve on the active shelf only");
    let (ok, _, stderr) = run(&dir, &["rm", "--archived", "a"]);
    assert!(ok, "stderr: {stderr}");
    assert!(archived_ids(&dir).is_empty());
}

#[test]
fn ls_records_a_died_capture_once_with_true_counts() {
    let dir = temp_data_dir();
    let id = spawn_and_kill(&dir, "doomed", b"one\ntwo\n");
    assert_eq!(read_meta(&dir, &id)["status"], "running");

    let (ok, listing, stderr) = run(&dir, &["ls"]);
    assert!(ok, "stderr: {stderr}");
    let row = listing.lines().find(|l| l.contains("doomed")).unwrap();
    assert!(row.contains("died"), "{listing}");
    assert!(row.contains(" 2 ") && row.contains("8B"), "{listing}");

    // The death is recorded, so later listings neither probe the pid nor
    // re-count the data.
    let meta = read_meta(&dir, &id);
    assert_eq!(meta["status"], "died");
    assert_eq!(meta["bytes"], 8);
    assert_eq!(meta["lines"], 2);
    assert!(meta["ended_at_ms"].is_i64(), "{meta}");
    let (_, again, _) = run(&dir, &["ls"]);
    assert!(
        again
            .lines()
            .any(|l| l.contains("doomed") && l.contains("died"))
    );
}

#[test]
fn cat_does_not_write() {
    let dir = temp_data_dir();
    let id = spawn_and_kill(&dir, "doomed", b"x\n");
    let before = std::fs::read(dir.join("streams").join(&id).join("meta.json")).unwrap();
    let (ok, data, stderr) = run(&dir, &["cat", "doomed"]);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(data, "x\n");
    let after = std::fs::read(dir.join("streams").join(&id).join("meta.json")).unwrap();
    assert_eq!(before, after, "cat is read-only");
}

#[test]
fn clean_dry_run_does_not_write_either() {
    let dir = temp_data_dir();
    let id = spawn_and_kill(&dir, "doomed", b"x\n");
    let before = std::fs::read(dir.join("streams").join(&id).join("meta.json")).unwrap();
    let (ok, stdout, stderr) = clean(&dir, &["--everything", "--dry-run"]);
    assert!(ok, "stderr: {stderr}");
    assert!(stdout.contains(&id), "dead captures are eligible: {stdout}");
    let after = std::fs::read(dir.join("streams").join(&id).join("meta.json")).unwrap();
    assert_eq!(before, after, "a dry run changes nothing");
}
