mod common;

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};

use common::{bin, lgt, read_meta, run_capture, stream_ids, temp_data_dir};

#[test]
fn cat_resolves_latest_by_name_and_exact_id() {
    let dir = temp_data_dir();
    run_capture(&dir, &["job"], b"first run\n");
    run_capture(&dir, &["job"], b"second run\n");

    let out = lgt(&dir, &["cat", "job"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, b"second run\n", "name must resolve to newest");

    let mut ids = stream_ids(&dir);
    ids.sort_by_key(|id| read_meta(&dir, id)["started_at_ms"].as_i64().unwrap());
    let out = lgt(&dir, &["cat", &ids[0]]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, b"first run\n", "exact id must win");

    let out = lgt(&dir, &["cat", "no-such-stream"]).output().unwrap();
    assert!(!out.status.success());
}

#[test]
fn pipeline_siblings_link_automatically() {
    let dir = temp_data_dir();
    // Both captures run from this test process, so they share a pgid — the
    // same situation as two lgt instances inside one shell pipeline.
    run_capture(&dir, &["step1"], b"a\n");
    run_capture(&dir, &["step2"], b"b\n");
    let ids = stream_ids(&dir);
    assert_eq!(ids.len(), 2);
    let g1 = read_meta(&dir, &ids[0])["group_id"].clone();
    let g2 = read_meta(&dir, &ids[1])["group_id"].clone();
    assert_eq!(g1, g2, "same-pgid captures must land in the same group");
}

#[test]
fn manual_group_links_across_pipelines() {
    let dir = temp_data_dir();
    run_capture(&dir, &["-g", "deploy", "one"], b"a\n");
    run_capture(&dir, &["-g", "deploy", "two"], b"b\n");
    for id in stream_ids(&dir) {
        let meta = read_meta(&dir, &id);
        assert_eq!(meta["group_id"], "named-deploy");
        assert_eq!(meta["group_name"], "deploy");
    }
}

#[test]
fn cat_follow_streams_a_live_capture_to_completion() {
    let dir = temp_data_dir();
    // Start a capture and keep its stdin open.
    let mut capture: Child = lgt(&dir, &["live"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut cap_in = capture.stdin.take().unwrap();
    cap_in.write_all(b"early\n").unwrap();
    cap_in.flush().unwrap();

    // Wait until the stream is registered.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while stream_ids(&dir).is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "stream never appeared"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let mut follower = lgt(&dir, &["cat", "-f", "live"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    // Feed more data while the follower is attached, then finish.
    std::thread::sleep(std::time::Duration::from_millis(300));
    cap_in.write_all(b"late\n").unwrap();
    drop(cap_in);
    assert!(capture.wait().unwrap().success());

    let mut followed = Vec::new();
    follower
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut followed)
        .unwrap();
    assert!(follower.wait().unwrap().success());
    assert_eq!(
        followed, b"early\nlate\n",
        "follow must deliver data written before and during the follow, then stop"
    );
}

#[test]
fn ls_rm_and_clean() {
    let dir = temp_data_dir();
    run_capture(&dir, &["keepme"], b"data\n");
    run_capture(&dir, &["dropme"], b"data\n");

    let out = lgt(&dir, &["ls"]).output().unwrap();
    assert!(out.status.success());
    let listing = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(listing.contains("keepme"));
    assert!(listing.contains("dropme"));
    assert!(listing.contains("done"));
    // Finalized counters are shown: 1 line, 5 bytes.
    assert!(listing.contains("5B"), "byte column wrong:\n{listing}");

    let out = lgt(&dir, &["rm", "dropme"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(stream_ids(&dir).len(), 1);

    let out = lgt(&dir, &["clean", "--everything"]).output().unwrap();
    assert!(out.status.success());
    assert!(stream_ids(&dir).is_empty());
}

#[test]
fn lgt_path_prints_the_data_dir() {
    let dir = temp_data_dir();
    let out = lgt(&dir, &["path"]).output().unwrap();
    assert!(out.status.success());
    let printed = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(printed, dir.display().to_string());
}

#[test]
fn rm_refuses_running_streams_without_force() {
    let dir = temp_data_dir();
    let mut child = lgt(&dir, &["busy"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"working\n").unwrap();
    stdin.flush().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while stream_ids(&dir).is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "stream never appeared"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let out = lgt(&dir, &["rm", "busy"]).output().unwrap();
    assert!(!out.status.success(), "rm must refuse a running stream");
    assert_eq!(stream_ids(&dir).len(), 1, "nothing may be removed");

    let out = lgt(&dir, &["rm", "-f", "busy"]).output().unwrap();
    assert!(out.status.success(), "rm -f must remove it");
    assert!(stream_ids(&dir).is_empty());

    // The capture keeps piping and exits cleanly even though its stream was
    // removed from underneath it.
    stdin.write_all(b"still alive\n").unwrap();
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "a removed capture must not break its pipeline"
    );
}

#[test]
fn lgt_group_env_links_captures() {
    let dir = temp_data_dir();
    for name in ["one", "two"] {
        let mut cmd = lgt(&dir, &[name]);
        cmd.env("LGT_GROUP", "scripted");
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"x\n").unwrap();
        assert!(child.wait().unwrap().success());
    }
    for id in stream_ids(&dir) {
        let meta = read_meta(&dir, &id);
        assert_eq!(meta["group_id"], "named-scripted");
        assert_eq!(meta["group_name"], "scripted");
    }
}

#[test]
fn lgt_data_dir_env_selects_the_store() {
    let dir = temp_data_dir();
    let mut cmd = Command::new(bin());
    cmd.arg("envdata");
    cmd.env("LGT_DATA_DIR", &dir);
    cmd.env_remove("LGT_GROUP");
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"x\n").unwrap();
    assert!(child.wait().unwrap().success());
    let ids = stream_ids(&dir);
    assert_eq!(ids.len(), 1);
    assert_eq!(read_meta(&dir, &ids[0])["name"], "envdata");
}

#[test]
fn concurrent_pipeline_instances_share_a_group() {
    let dir = temp_data_dir();
    // Two lgt processes alive at the same time, as in a real
    // `cmd | lgt | cmd | lgt` pipeline: both race for the same pgid claim.
    let mut children = Vec::new();
    for name in ["left", "right"] {
        let mut child = lgt(&dir, &[name])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        children.push((child, stdin));
    }
    for (child, mut stdin) in children {
        stdin.write_all(b"x\n").unwrap();
        drop(stdin);
        let mut child = child;
        assert!(child.wait().unwrap().success());
    }
    let ids = stream_ids(&dir);
    assert_eq!(ids.len(), 2);
    let g1 = read_meta(&dir, &ids[0])["group_id"].clone();
    let g2 = read_meta(&dir, &ids[1])["group_id"].clone();
    assert_eq!(g1, g2, "concurrent captures must join one group");
}

#[test]
fn tui_refuses_to_run_without_a_terminal() {
    let dir = temp_data_dir();
    let out = lgt(&dir, &["tui"]).output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("terminal"), "stderr: {stderr}");
}

#[test]
fn global_flag_before_subcommand_still_selects_the_subcommand() {
    let dir = temp_data_dir();
    run_capture(&dir, &["job"], b"payload\n");
    let mut cmd = Command::new(bin());
    cmd.arg("--data-dir").arg(&dir).args(["cat", "job"]);
    cmd.env_remove("LGT_DATA_DIR");
    cmd.env_remove("LGT_GROUP");
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"payload\n");
}
