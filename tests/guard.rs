//! A bare subcommand (`lgt ls`, `lgt archive`) with live piped input is
//! almost certainly a capture whose name collides with a subcommand, and
//! running it would swallow the pipeline's output — so it is refused.
//! Anything with arguments is deliberate and runs (`… | while read id; do
//! lgt rm "$id"; done`), and the empty closed pipe cron gives its jobs is
//! fine either way.

mod common;

use common::{
    DAY_MS, FakeState, archived_ids, fake_stream, now_ms, run_capture, run_piped, stream_ids,
    temp_data_dir,
};

#[test]
fn bare_subcommands_refuse_live_piped_input() {
    let dir = temp_data_dir();
    run_capture(&dir, &["job"], b"payload\n");
    fake_stream(
        &dir,
        "s-old",
        "old",
        now_ms() - 2 * DAY_MS,
        FakeState::Done,
        10,
    );
    for args in [
        &["ls"][..],
        &["path"][..],
        &["tui"][..],
        &["clean"][..],
        &["archive"][..],
    ] {
        let (ok, stdout, stderr) = run_piped(&dir, args, b"important output\n");
        assert!(!ok, "{args:?}: piped output would be swallowed");
        assert!(stderr.contains("does not read"), "{args:?}: {stderr}");
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
    }
    assert_eq!(stream_ids(&dir).len(), 2, "nothing removed");
    assert!(archived_ids(&dir).is_empty(), "nothing archived");
}

#[test]
fn subcommands_with_arguments_are_deliberate_and_run_with_piped_input() {
    // `lgt ls | … | while read id; do lgt rm "$id"; done`: the inner lgt has
    // the loop's input on stdin, and must neither refuse nor eat it.
    let dir = temp_data_dir();
    run_capture(&dir, &["job"], b"payload\n");
    fake_stream(
        &dir,
        "s-old",
        "old",
        now_ms() - 40 * DAY_MS,
        FakeState::Done,
        10,
    );
    let (ok, data, stderr) = run_piped(&dir, &["cat", "job"], b"s-1\ns-2\n");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(data, "payload\n");
    let (ok, _, stderr) = run_piped(&dir, &["rm", "old"], b"s-1\ns-2\n");
    assert!(ok, "stderr: {stderr}");
    let (ok, _, stderr) = run_piped(&dir, &["clean", "--everything", "--dry-run"], b"x\n");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stream_ids(&dir).len(), 1);
}

#[test]
fn an_empty_closed_pipe_on_stdin_is_accepted_like_under_cron() {
    let dir = temp_data_dir();
    // cron starts jobs with an empty pipe on stdin whose writer is closed.
    let (reader, writer) = std::io::pipe().unwrap();
    drop(writer);
    let out = common::lgt(&dir, &["ls"]).stdin(reader).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_writer_that_closes_right_after_starting_us_is_accepted() {
    // cron closes its end of the job's stdin just after forking the job.
    let dir = temp_data_dir();
    let (reader, writer) = std::io::pipe().unwrap();
    let child = common::lgt(&dir, &["ls"])
        .stdin(reader)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(writer);
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_pipe_whose_writer_stays_open_is_refused_promptly() {
    let dir = temp_data_dir();
    // `slow-producer | lgt ls`: nothing written yet, writer alive.
    let (reader, writer) = std::io::pipe().unwrap();
    let started = std::time::Instant::now();
    let out = common::lgt(&dir, &["ls"]).stdin(reader).output().unwrap();
    drop(writer);
    assert!(!out.status.success());
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}
