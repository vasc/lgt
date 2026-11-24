use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};

pub use crate::meta::{Liveness, PipelineProc, Status, StreamMeta};
pub use crate::settle::Observed;

/// Which directory of the store a [`Store`] handle operates on. Captures
/// always land on the active shelf; `lgt archive` moves finished streams to
/// the archive shelf, which default listings and the TUI never scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shelf {
    Active,
    Archive,
}

/// Identity of a meta file's current contents (inode, length, mtime), used
/// to skip re-parsing unchanged files. Every meta write is a rename, so the
/// inode changes even where mtimes are coarse.
pub type MetaStamp = (u64, u64, Option<SystemTime>);

#[derive(Clone)]
pub struct Store {
    root: PathBuf,
    shelf: Shelf,
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn system_time_ms(time: SystemTime) -> Option<i64> {
    let since_epoch = time.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    i64::try_from(since_epoch.as_millis()).ok()
}

impl Store {
    /// Open the store rooted at `root`, on the active shelf. The archive
    /// directory is only created when something is first archived, so
    /// opening a read-only store never writes.
    pub fn open(root: PathBuf) -> Result<Self> {
        let store = Store {
            root,
            shelf: Shelf::Active,
        };
        for dir in [store.streams_dir(), store.groups_dir()] {
            fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        Ok(store)
    }

    /// A handle to the same store operating on another shelf.
    pub fn on(&self, shelf: Shelf) -> Store {
        Store {
            root: self.root.clone(),
            shelf,
        }
    }

    pub fn shelf(&self) -> Shelf {
        self.shelf
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn streams_dir(&self) -> PathBuf {
        match self.shelf {
            Shelf::Active => self.root.join("streams"),
            Shelf::Archive => self.root.join("archive"),
        }
    }

    pub fn groups_dir(&self) -> PathBuf {
        self.root.join("groups")
    }

    pub fn stream_dir(&self, id: &str) -> PathBuf {
        self.streams_dir().join(id)
    }

    pub fn data_path(&self, id: &str) -> PathBuf {
        self.stream_dir(id).join("data.log")
    }

    pub fn meta_path(&self, id: &str) -> PathBuf {
        self.stream_dir(id).join("meta.json")
    }

    pub fn new_stream_id(&self, pid: i32, now_ms: i64) -> String {
        format!("s-{}-{}", crate::timefmt::compact(now_ms), pid)
    }

    pub fn create_stream_dir(&self, id: &str) -> Result<()> {
        fs::create_dir_all(self.stream_dir(id))
            .with_context(|| format!("creating stream dir for {id}"))
    }

    /// Atomically (write temp + rename) publish a stream's metadata so
    /// concurrent readers (the TUI) never observe a half-written file. The
    /// temp name is unique per writer process and call, because recording a
    /// death can race the capture's own writes.
    pub fn write_meta(&self, meta: &StreamMeta) -> Result<()> {
        let tmp = self.stream_dir(&meta.id).join(format!(
            ".meta-{}-{}.tmp",
            crate::proc::my_pid(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let body = serde_json::to_vec_pretty(meta)?;
        fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, self.meta_path(&meta.id))
            .inspect_err(|_| {
                let _ = fs::remove_file(&tmp);
            })
            .with_context(|| format!("publishing meta for {}", meta.id))
    }

    pub fn read_meta(&self, id: &str) -> Result<StreamMeta> {
        let path = self.meta_path(id);
        let body = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_slice(&body).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn meta_stamp(&self, id: &str) -> Option<MetaStamp> {
        let meta = fs::metadata(self.meta_path(id)).ok()?;
        Some((meta.ino(), meta.len(), meta.modified().ok()))
    }

    /// Every stream on this shelf, observed read-only.
    pub fn list_observed(&self) -> Result<Vec<Observed>> {
        let dir = self.streams_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("listing {}", dir.display())),
        };
        let mut observed = Vec::new();
        for entry in entries {
            let entry = entry?;
            let id = entry.file_name();
            let Some(id) = id.to_str() else { continue };
            if let Ok(meta) = self.read_meta(id) {
                observed.push(self.observe(meta));
            }
        }
        Ok(observed)
    }

    pub fn list(&self) -> Result<Vec<StreamMeta>> {
        Ok(self.list_observed()?.into_iter().map(|o| o.meta).collect())
    }

    /// Resolve a reference: an exact stream id wins, otherwise the most
    /// recently started stream carrying that name.
    pub fn resolve(&self, reference: &str) -> Result<StreamMeta> {
        if self.meta_path(reference).exists() {
            return Ok(self.observe(self.read_meta(reference)?).meta);
        }
        let mut named: Vec<StreamMeta> = self
            .list()?
            .into_iter()
            .filter(|m| m.name.as_deref() == Some(reference))
            .collect();
        named.sort_by_key(|m| m.started_at_ms);
        let shelf = match self.shelf {
            Shelf::Active => "",
            Shelf::Archive => " in the archive",
        };
        named
            .pop()
            .with_context(|| format!("no stream with id or name `{reference}`{shelf}"))
    }

    pub fn remove_stream(&self, id: &str) -> Result<()> {
        let dir = self.stream_dir(id);
        // A concurrent meta writer can drop a temp file into the directory
        // while it is being emptied; one retry clears it.
        fs::remove_dir_all(&dir)
            .or_else(|_| fs::remove_dir_all(&dir))
            .with_context(|| format!("removing stream {id}"))
    }

    /// Move a stream to another shelf of the same store (a single rename).
    pub fn move_stream(&self, id: &str, to: &Store) -> Result<()> {
        let destination = to.stream_dir(id);
        if destination.exists() {
            bail!("stream {id} already exists in the destination");
        }
        fs::create_dir_all(to.streams_dir())
            .with_context(|| format!("creating {}", to.streams_dir().display()))?;
        fs::rename(self.stream_dir(id), &destination)
            .with_context(|| format!("moving stream {id}"))?;
        // A meta temp file inside the moved directory can only belong to a
        // writer whose rename target (the old path) is gone: it is orphaned.
        to.sweep_meta_temps(id, std::time::Duration::ZERO);
        Ok(())
    }

    /// When a stream last wrote. Ages ("older than 24h") are measured from
    /// here: a long job that just finished is not old, and neither is a busy
    /// capture — but one stuck without output for weeks is.
    pub fn last_activity_ms(&self, meta: &StreamMeta) -> i64 {
        meta.finished_at_ms().unwrap_or_else(|| {
            fs::metadata(self.data_path(&meta.id))
                .and_then(|m| m.modified())
                .ok()
                .and_then(system_time_ms)
                .unwrap_or(meta.started_at_ms)
        })
    }

    pub fn data_len(&self, id: &str) -> u64 {
        fs::metadata(self.data_path(id))
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// Disk footprint of a stream: every file in its directory.
    pub fn stream_size(&self, id: &str) -> u64 {
        let Ok(entries) = fs::read_dir(self.stream_dir(id)) else {
            return 0;
        };
        entries
            .flatten()
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum()
    }

    /// Byte and line counts read from the data file itself. This is the
    /// truth for streams that never finalized (still running, or killed):
    /// their meta counters are stale by design.
    pub fn data_stats(&self, id: &str) -> (u64, u64) {
        use std::io::Read;
        let Ok(mut file) = fs::File::open(self.data_path(id)) else {
            return (0, 0);
        };
        let mut buf = vec![0u8; 256 * 1024];
        let mut bytes: u64 = 0;
        let mut lines: u64 = 0;
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    bytes += u64::try_from(n).unwrap_or(0);
                    lines += u64::try_from(buf[..n].iter().filter(|b| **b == b'\n').count())
                        .unwrap_or(0);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        (bytes, lines)
    }
}

#[cfg(test)]
#[path = "store_testutil.rs"]
pub mod testutil;

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
