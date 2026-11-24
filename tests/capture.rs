mod common;

use std::fs;
use std::io::Write;
use std::process::Stdio;

use common::{lgt, read_meta, run_capture, stream_ids, temp_data_dir};

#[test]
fn passthrough_and_capture_with_name() {
    let dir = temp_data_dir();
    let input = b"hello\nworld\n";
    let out = run_capture(&dir, &["greeting"], input);
    assert_eq!(out, input, "stdin must be piped back out unchanged");

    let ids = stream_ids(&dir);
    assert_eq!(ids.len(), 1);
    let id = &ids[0];
    let data = fs::read(dir.join("streams").join(id).join("data.log")).unwrap();
    assert_eq!(data, input, "captured log must match the piped input");

    let meta = read_meta(&dir, id);
    assert_eq!(meta["name"], "greeting");
    assert_eq!(meta["status"], "done");
    assert_eq!(meta["bytes"], 12);
    assert_eq!(meta["lines"], 2);
    assert!(meta["ended_at_ms"].is_i64());
}

#[test]
fn binary_data_survives_roundtrip() {
    let dir = temp_data_dir();
    let mut input = Vec::new();
    for _ in 0..3 {
        for b in 0..=255u8 {
            input.push(b);
        }
    }
    let out = run_capture(&dir, &[], &input);
    assert_eq!(out, input);
    let ids = stream_ids(&dir);
    let data = fs::read(dir.join("streams").join(&ids[0]).join("data.log")).unwrap();
    assert_eq!(data, input);
}

#[test]
fn invalid_names_are_rejected() {
    let dir = temp_data_dir();
    let mut child = lgt(&dir, &["bad name"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success());
    assert!(stream_ids(&dir).is_empty(), "no stream should be created");
}

#[test]
fn broken_downstream_stops_capture_like_tee() {
    let dir = temp_data_dir();
    let mut child = lgt(&dir, &[])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Simulate `lgt | head -c0`: downstream goes away immediately.
    drop(child.stdout.take());
    let mut stdin = child.stdin.take().unwrap();
    let chunk = vec![b'x'; 64 * 1024];
    // lgt should exit once it notices stdout is gone; our writes then fail
    // with a broken pipe. Bound the loop so a regression can't hang the test.
    let mut wrote_err = false;
    for _ in 0..256 {
        if stdin.write_all(&chunk).is_err() {
            wrote_err = true;
            break;
        }
    }
    drop(stdin);
    let status = child.wait().unwrap();
    // tee dies of SIGPIPE (status 141) in this situation; lgt reports the
    // same so `set -o pipefail` treats both identically.
    assert_eq!(status.code(), Some(141));
    assert!(wrote_err, "lgt kept reading after downstream closed");
    let ids = stream_ids(&dir);
    assert_eq!(read_meta(&dir, &ids[0])["status"], "done");
}

#[test]
fn drain_keeps_capturing_after_downstream_closes() {
    let dir = temp_data_dir();
    let mut child = lgt(&dir, &["--drain"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let mut stdin = child.stdin.take().unwrap();
    let payload = vec![b'y'; 256 * 1024];
    stdin.write_all(&payload).unwrap();
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success());
    let ids = stream_ids(&dir);
    let data = fs::read(dir.join("streams").join(&ids[0]).join("data.log")).unwrap();
    assert_eq!(data.len(), payload.len(), "--drain must capture everything");
}

#[test]
fn out_flag_writes_a_local_copy_too() {
    let dir = temp_data_dir();
    let copy_path = dir.join("local-copy.log");
    let copy_arg = copy_path.display().to_string();
    let out = run_capture(&dir, &["copied", "-o", &copy_arg], b"kept twice\n");
    assert_eq!(out, b"kept twice\n");
    assert_eq!(fs::read(&copy_path).unwrap(), b"kept twice\n");
    let ids = stream_ids(&dir);
    let data = fs::read(dir.join("streams").join(&ids[0]).join("data.log")).unwrap();
    assert_eq!(data, b"kept twice\n", "the store copy is still primary");
    assert_eq!(read_meta(&dir, &ids[0])["copy"], copy_arg);
}

#[test]
fn pipeline_snapshot_is_recorded() {
    let dir = temp_data_dir();
    run_capture(&dir, &["snap"], b"x\n");
    let ids = stream_ids(&dir);
    let meta = read_meta(&dir, &ids[0]);
    let pipeline = meta["pipeline"].as_array().expect("pipeline snapshot");
    // The test process itself is part of the capture's process group.
    let me = i64::from(std::process::id());
    assert!(
        pipeline.iter().any(|p| p["pid"].as_i64() == Some(me)),
        "snapshot should include the test process ({me}): {pipeline:?}"
    );
}
