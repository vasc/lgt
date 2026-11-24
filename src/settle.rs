//! Seeing through dead captures: a stream whose meta says `running` but
//! whose capture process is gone. Observing never writes; recording the
//! death (counting its lines, writing `died`) is a separate, explicit step.

use std::fs;
use std::time::{Duration, SystemTime};

use crate::store::{Liveness, Status, Store, StreamMeta, system_time_ms};

/// A stream's metadata as currently observed, without writing anything.
pub struct Observed {
    pub meta: StreamMeta,
    /// The capture process turned out to be gone and `meta` was marked
    /// `Died` in memory only: its `lines` are stale and nothing is on disk
    /// yet (see [`Store::record_death`]).
    pub died_unrecorded: bool,
}

/// Meta temp files older than this are leftovers of an interrupted writer.
const STALE_TEMP: Duration = Duration::from_secs(60);

impl Store {
    /// See a stream as it really is, without writing: a `Running` meta whose
    /// capture process is gone is returned as `Died`, with its true size and
    /// end time (the data file's mtime). Cheap: one `stat`, no data read.
    pub fn observe(&self, meta: StreamMeta) -> Observed {
        if meta.status != Status::Running || meta.liveness() == Liveness::Running {
            return Observed {
                meta,
                died_unrecorded: false,
            };
        }
        // The process may have finalized the meta itself between our read and
        // the probe; now that it is gone the on-disk copy is final.
        let mut fresh = match self.read_meta(&meta.id) {
            Ok(fresh) if fresh.status != Status::Running => {
                return Observed {
                    meta: fresh,
                    died_unrecorded: false,
                };
            }
            Ok(fresh) => fresh,
            Err(_) => meta,
        };
        let data = fs::metadata(self.data_path(&fresh.id)).ok();
        fresh.status = Status::Died;
        fresh.bytes = data.as_ref().map(|m| m.len()).unwrap_or(0);
        if fresh.ended_at_ms.is_none() {
            fresh.ended_at_ms = data
                .and_then(|m| m.modified().ok())
                .and_then(system_time_ms);
        }
        Observed {
            meta: fresh,
            died_unrecorded: true,
        }
    }

    /// Record on disk that a stream's capture died: count its lines once and
    /// write `died`, so nobody has to probe its pid or scan its data again.
    /// Re-checks everything first, so it is safe to call late or twice.
    /// Returns the died meta with its true counts — also when writing it
    /// fails (a read-only store), so callers never count twice — or `None`
    /// when there was nothing to record.
    pub fn record_death(&self, id: &str) -> Option<StreamMeta> {
        let observed = self.observe(self.read_meta(id).ok()?);
        if !observed.died_unrecorded {
            return None;
        }
        let mut meta = observed.meta;
        let (bytes, lines) = self.data_stats(id);
        meta.bytes = bytes;
        meta.lines = lines;
        self.sweep_meta_temps(id, STALE_TEMP);
        // Best effort: on a read-only store the counts are still right.
        let _ = self.write_meta(&meta);
        Some(meta)
    }

    /// Remove meta temp files in a stream directory that are older than
    /// `min_age`: leftovers of a writer that was interrupted (process exit)
    /// or raced a move. Best effort.
    pub fn sweep_meta_temps(&self, id: &str, min_age: Duration) {
        let Ok(entries) = fs::read_dir(self.stream_dir(id)) else {
            return;
        };
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !(name.starts_with(".meta-") && name.ends_with(".tmp")) {
                continue;
            }
            let old_enough = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age >= min_age);
            if old_enough {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}
