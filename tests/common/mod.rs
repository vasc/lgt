// Each integration-test crate compiles this module separately and uses a
// different subset of the helpers.
#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

pub fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_lgt")
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn temp_data_dir() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let crate_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("lgt-it-{crate_name}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A `lgt` invocation with `args...` followed by `--data-dir`. The data-dir
/// flag is global, so it is valid in this trailing position for both capture
/// mode and every subcommand.
pub fn lgt(data_dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(bin());
    cmd.args(args);
    cmd.arg("--data-dir").arg(data_dir);
    cmd.env_remove("LGT_DATA_DIR");
    cmd.env_remove("LGT_GROUP");
    cmd
}

/// Pipe `input` through `lgt args...` and return its stdout.
pub fn run_capture(data_dir: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = lgt(data_dir, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let owned = input.to_vec();
    let writer = std::thread::spawn(move || {
        stdin.write_all(&owned).unwrap();
    });
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert!(out.status.success(), "lgt exited with {:?}", out.status);
    out.stdout
}

pub fn stream_ids(data_dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(data_dir.join("streams")) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids
}

/// Run `lgt args...` with stdin closed; returns (success, stdout, stderr).
pub fn run(data_dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = lgt(data_dir, args).stdin(Stdio::null()).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Run `lgt args...` with `input` piped into stdin, the way `cmd | lgt args`
/// would; returns (success, stdout, stderr).
pub fn run_piped(data_dir: &Path, args: &[&str], input: &[u8]) -> (bool, String, String) {
    let mut child = lgt(data_dir, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // The child may refuse and exit without reading; a broken pipe is fine.
    let _ = stdin.write_all(input);
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

pub fn archived_ids(data_dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(data_dir.join("archive")) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids
}

/// Start a capture named `name`, feed it `data`, wait until the data has
/// landed, then SIGKILL it so it never finalizes. Returns the stream id.
pub fn spawn_and_kill(data_dir: &Path, name: &str, data: &[u8]) -> String {
    let mut child = lgt(data_dir, &[name])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(data).unwrap();
    stdin.flush().unwrap();
    let expected = u64::try_from(data.len()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let id = loop {
        if let Some(id) = stream_ids(data_dir).into_iter().find(|id| {
            fs::metadata(data_dir.join("streams").join(id).join("data.log"))
                .map(|m| m.len())
                .unwrap_or(0)
                == expected
        }) {
            break id;
        }
        assert!(std::time::Instant::now() < deadline, "capture never landed");
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stdin);
    id
}

pub fn read_meta(data_dir: &Path, id: &str) -> serde_json::Value {
    let body = fs::read(data_dir.join("streams").join(id).join("meta.json")).unwrap();
    serde_json::from_slice(&body).unwrap()
}

pub fn now_ms() -> i64 {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    i64::try_from(since_epoch.as_millis()).unwrap()
}

pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// How a fabricated stream should look to lgt.
pub enum FakeState {
    Done,
    /// Meta says running and the recorded pid is this test process, so lgt
    /// sees it as genuinely running.
    Running,
}

/// Write a stream directly into the store layout, so tests can control its
/// age and size without waiting real time.
pub fn fake_stream(
    data_dir: &Path,
    id: &str,
    name: &str,
    started_at_ms: i64,
    state: FakeState,
    data_bytes: usize,
) {
    let dir = data_dir.join("streams").join(id);
    fs::create_dir_all(&dir).unwrap();
    let data = dir.join("data.log");
    fs::write(&data, vec![b'x'; data_bytes]).unwrap();
    let (status, pid, ended, last_write) = match state {
        FakeState::Done => (
            "done",
            1,
            serde_json::json!(started_at_ms + 1_000),
            started_at_ms + 1_000,
        ),
        FakeState::Running => (
            "running",
            i32::try_from(std::process::id()).unwrap(),
            serde_json::Value::Null,
            started_at_ms,
        ),
    };
    // The data file was last written when the stream was last active.
    let last_write = std::time::UNIX_EPOCH
        + std::time::Duration::from_millis(u64::try_from(last_write.max(0)).unwrap());
    fs::File::options()
        .write(true)
        .open(&data)
        .unwrap()
        .set_modified(last_write)
        .unwrap();
    let meta = serde_json::json!({
        "id": id,
        "name": name,
        "group_id": format!("g-{id}"),
        "group_name": null,
        "pid": pid,
        "pgid": pid,
        "started_at_ms": started_at_ms,
        "ended_at_ms": ended,
        "status": status,
        "bytes": data_bytes,
        "lines": 0,
        "cwd": null,
        "copy": null,
        "pipeline": null,
    });
    fs::write(dir.join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
}
