use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::store::{Status, Store, StreamMeta, validate_stream_name};
use crate::{group, proc, timefmt};

pub struct CaptureOpts {
    pub name: Option<String>,
    pub group: Option<String>,
    pub drain: bool,
    /// Also write the captured bytes to this file (classic tee behavior).
    pub out: Option<std::path::PathBuf>,
}

/// How the capture ended. `DownstreamClosed` means we stopped because the
/// read side of our stdout went away (tee dies of SIGPIPE there; the caller
/// mirrors that with exit status 141 so `pipefail` pipelines behave the same
/// with `lgt` as with `tee`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureOutcome {
    Completed,
    DownstreamClosed,
}

/// The tee loop: everything read from stdin is appended to the stream's data
/// file and echoed to stdout. Metadata is published up front so the TUI can
/// watch the stream while it is still being written.
pub fn run(store: &Store, opts: CaptureOpts) -> Result<CaptureOutcome> {
    if let Some(name) = &opts.name {
        validate_stream_name(name)?;
    }
    let pid = proc::my_pid();
    let pgid = proc::my_pgid();
    let group = group::resolve(store, pid, pgid, opts.group)?;
    group::prune_stale_claims(store, timefmt::now_ms());
    let started_at_ms = timefmt::now_ms();
    let id = store.new_stream_id(pid, started_at_ms);

    let meta = StreamMeta {
        id: id.clone(),
        name: opts.name,
        group_id: group.id,
        group_name: group.name,
        pid,
        pgid,
        started_at_ms,
        ended_at_ms: None,
        status: Status::Running,
        bytes: 0,
        lines: 0,
        cwd: std::env::current_dir()
            .ok()
            .map(|p| p.display().to_string()),
        copy: opts.out.as_ref().map(|p| p.display().to_string()),
        pipeline: None,
    };
    store.create_stream_dir(&id)?;
    let mut data = std::fs::File::create(store.data_path(&id))
        .with_context(|| format!("creating {}", store.data_path(&id).display()))?;
    let mut copy = match &opts.out {
        Some(path) => Some(
            std::fs::File::create(path)
                .with_context(|| format!("creating copy file {}", path.display()))?,
        ),
        None => None,
    };
    store.write_meta(&meta)?;

    let shared = Arc::new(Mutex::new(meta));

    // Label the pipeline in the background. Two snapshots are merged: one
    // right away (short-lived stages like `seq` may already be gone 150ms in)
    // and one after the pipeline has settled (processes that had forked but
    // not yet exec'd at t=0 now show their real command).
    {
        let shared = Arc::clone(&shared);
        let store = store.clone();
        std::thread::spawn(move || {
            let early = proc::pipeline_snapshot(pgid);
            publish_snapshot(&shared, &store, early);
            std::thread::sleep(Duration::from_millis(150));
            let settled = proc::pipeline_snapshot(pgid);
            publish_snapshot(&shared, &store, settled);
        });
    }

    let mut bytes: u64 = 0;
    let mut lines: u64 = 0;
    let pump_result = pump(&mut data, copy.as_mut(), opts.drain, &mut bytes, &mut lines);

    let mut meta = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if meta.pipeline.is_none() {
        // The pipeline finished before the background snapshot landed; take
        // one now, while our siblings may still be alive.
        let snapshot = proc::pipeline_snapshot(pgid);
        if !snapshot.is_empty() {
            meta.pipeline = Some(snapshot);
        }
    }
    meta.status = Status::Done;
    meta.ended_at_ms = Some(timefmt::now_ms());
    meta.bytes = bytes;
    meta.lines = lines;
    let finalize = store.write_meta(&meta);
    drop(meta);
    if let Err(err) = finalize {
        if store.stream_dir(&id).exists() {
            // A real write failure — but never let it mask a pump error.
            pump_result?;
            return Err(err);
        }
        // The stream was removed while we were running (`lgt rm -f` /
        // `lgt clean --all`). Losing the log was the user's call; the
        // pipeline itself must keep working.
        eprintln!("lgt: capture {id} was removed while running; log discarded");
    }
    pump_result
}

/// Merge a fresh process-group snapshot into the shared meta and publish it.
/// Merging (rather than replacing) keeps entries for stages that exited
/// between snapshots and lets later snapshots refine command lines.
fn publish_snapshot(
    shared: &Arc<Mutex<StreamMeta>>,
    store: &Store,
    snapshot: Vec<crate::store::PipelineProc>,
) {
    if snapshot.is_empty() {
        return;
    }
    if let Ok(mut meta) = shared.lock() {
        let merged = match meta.pipeline.take() {
            Some(existing) => proc::merge_snapshots(existing, snapshot),
            None => snapshot,
        };
        meta.pipeline = Some(merged);
        let _ = store.write_meta(&meta);
    }
}

fn pump(
    data: &mut std::fs::File,
    mut copy: Option<&mut std::fs::File>,
    drain: bool,
    bytes: &mut u64,
    lines: &mut u64,
) -> Result<CaptureOutcome> {
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let mut buf = vec![0u8; 64 * 1024];
    let mut out_open = true;
    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) => {
                return Ok(if out_open {
                    CaptureOutcome::Completed
                } else {
                    CaptureOutcome::DownstreamClosed
                });
            }
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e).context("reading stdin"),
        };
        let chunk = &buf[..n];
        data.write_all(chunk).context("writing capture file")?;
        if let Some(copy) = &mut copy {
            copy.write_all(chunk).context("writing copy file")?;
        }
        *bytes += u64::try_from(n).unwrap_or(0);
        *lines += u64::try_from(chunk.iter().filter(|b| **b == b'\n').count()).unwrap_or(0);
        if out_open {
            let wrote = stdout.write_all(chunk).and_then(|()| stdout.flush());
            match wrote {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                    // Downstream hung up. Match tee and stop (letting the
                    // upstream see the broken pipe too), unless --drain asked
                    // us to keep capturing to the log until upstream EOF.
                    out_open = false;
                    if !drain {
                        return Ok(CaptureOutcome::DownstreamClosed);
                    }
                }
                Err(e) => return Err(e).context("writing stdout"),
            }
        }
    }
}
