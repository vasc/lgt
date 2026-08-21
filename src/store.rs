use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Status as recorded by the capturing process itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Running,
    Done,
}

/// Status as observed from outside: a stream whose meta still says `Running`
/// but whose pid is gone was killed before it could finalize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Running,
    Done,
    Died,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineProc {
    pub pid: i32,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamMeta {
    pub id: String,
    pub name: Option<String>,
    pub group_id: String,
    pub group_name: Option<String>,
    pub pid: i32,
    pub pgid: i32,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub status: Status,
    pub bytes: u64,
    pub lines: u64,
    pub cwd: Option<String>,
    /// Path of the `-o/--out` local copy, when one was requested.
    #[serde(default)]
    pub copy: Option<String>,
    pub pipeline: Option<Vec<PipelineProc>>,
}

impl StreamMeta {
    pub fn liveness(&self) -> Liveness {
        match self.status {
            Status::Done => Liveness::Done,
            Status::Running => {
                if crate::proc::alive(self.pid) {
                    Liveness::Running
                } else {
                    Liveness::Died
                }
            }
        }
    }

    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open(root: PathBuf) -> Result<Self> {
        let store = Store { root };
        fs::create_dir_all(store.streams_dir())
            .with_context(|| format!("creating {}", store.streams_dir().display()))?;
        fs::create_dir_all(store.groups_dir())
            .with_context(|| format!("creating {}", store.groups_dir().display()))?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn streams_dir(&self) -> PathBuf {
        self.root.join("streams")
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
    /// concurrent readers (the TUI) never observe a half-written file.
    pub fn write_meta(&self, meta: &StreamMeta) -> Result<()> {
        let tmp = self
            .stream_dir(&meta.id)
            .join(format!(".meta-{}.tmp", meta.pid));
        let body = serde_json::to_vec_pretty(meta)?;
        fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, self.meta_path(&meta.id))
            .with_context(|| format!("publishing meta for {}", meta.id))
    }

    pub fn read_meta(&self, id: &str) -> Result<StreamMeta> {
        let path = self.meta_path(id);
        let body = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_slice(&body).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn list(&self) -> Result<Vec<StreamMeta>> {
        let mut metas = Vec::new();
        for entry in fs::read_dir(self.streams_dir())? {
            let entry = entry?;
            let id = entry.file_name();
            let Some(id) = id.to_str() else { continue };
            if let Ok(meta) = self.read_meta(id) {
                metas.push(meta);
            }
        }
        Ok(metas)
    }

    /// Resolve a reference: an exact stream id wins, otherwise the most
    /// recently started stream carrying that name.
    pub fn resolve(&self, reference: &str) -> Result<StreamMeta> {
        if self.meta_path(reference).exists() {
            return self.read_meta(reference);
        }
        let mut named: Vec<StreamMeta> = self
            .list()?
            .into_iter()
            .filter(|m| m.name.as_deref() == Some(reference))
            .collect();
        named.sort_by_key(|m| m.started_at_ms);
        named
            .pop()
            .with_context(|| format!("no stream with id or name `{reference}`"))
    }

    pub fn remove_stream(&self, id: &str) -> Result<()> {
        fs::remove_dir_all(self.stream_dir(id)).with_context(|| format!("removing stream {id}"))
    }

    pub fn data_len(&self, id: &str) -> u64 {
        fs::metadata(self.data_path(id))
            .map(|m| m.len())
            .unwrap_or(0)
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

/// Subcommand words can never name a capture: `lgt ls` must always mean the
/// subcommand, never "capture a stream called ls".
const RESERVED_NAMES: [&str; 7] = ["tui", "ls", "cat", "rm", "clean", "path", "help"];

/// Validation for capture names, which share the root command line with
/// subcommands.
pub fn validate_stream_name(name: &str) -> Result<()> {
    validate_name(name)?;
    if RESERVED_NAMES.contains(&name) {
        bail!(
            "`{name}` is a reserved command word and cannot be used as a capture name; \
             if you meant the `lgt {name}` subcommand, put options after it"
        );
    }
    Ok(())
}

/// Names are referenced from the command line and shown in the TUI; keep them
/// simple so they never need quoting.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("name must not be empty");
    }
    if name.starts_with('-') {
        bail!("name must not start with `-`");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        bail!("name may only contain letters, digits, `.`, `_` and `-`");
    }
    Ok(())
}

#[cfg(test)]
pub mod testutil {
    use super::Store;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    pub fn temp_store() -> Store {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("lgt-unit-{}-{n}", std::process::id()));
        Store::open(dir).expect("temp store")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(store: &Store, name: Option<&str>, started_at_ms: i64) -> StreamMeta {
        let id = store.new_stream_id(crate::proc::my_pid(), started_at_ms);
        StreamMeta {
            id,
            name: name.map(str::to_string),
            group_id: "g-test".to_string(),
            group_name: None,
            pid: 1,
            pgid: 1,
            started_at_ms,
            ended_at_ms: Some(started_at_ms),
            status: Status::Done,
            bytes: 0,
            lines: 0,
            cwd: None,
            copy: None,
            pipeline: None,
        }
    }

    #[test]
    fn meta_roundtrip() {
        let store = testutil::temp_store();
        let m = meta(&store, Some("build"), 1_000);
        store.create_stream_dir(&m.id).unwrap();
        store.write_meta(&m).unwrap();
        let back = store.read_meta(&m.id).unwrap();
        assert_eq!(back.id, m.id);
        assert_eq!(back.name.as_deref(), Some("build"));
        assert_eq!(back.status, Status::Done);
    }

    #[test]
    fn resolve_prefers_exact_id_then_latest_name() {
        let store = testutil::temp_store();
        let older = meta(&store, Some("x"), 1_000);
        let newer = meta(&store, Some("x"), 2_000);
        for m in [&older, &newer] {
            store.create_stream_dir(&m.id).unwrap();
            store.write_meta(m).unwrap();
        }
        assert_eq!(store.resolve("x").unwrap().id, newer.id);
        assert_eq!(store.resolve(&older.id).unwrap().id, older.id);
        assert!(store.resolve("missing").is_err());
    }

    #[test]
    fn dead_running_stream_reports_died() {
        let store = testutil::temp_store();
        let mut m = meta(&store, None, 1_000);
        m.status = Status::Running;
        m.pid = -1;
        assert_eq!(m.liveness(), Liveness::Died);
        m.pid = crate::proc::my_pid();
        assert_eq!(m.liveness(), Liveness::Running);
    }

    #[test]
    fn name_validation() {
        assert!(validate_name("build.step_1-a").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("-x").is_err());
        assert!(validate_name("a b").is_err());
        assert!(validate_name("a/b").is_err());
    }

    #[test]
    fn stream_names_exclude_command_words() {
        assert!(validate_stream_name("build").is_ok());
        for reserved in ["tui", "ls", "cat", "rm", "clean", "path", "help"] {
            assert!(validate_stream_name(reserved).is_err(), "{reserved}");
        }
        // Group names are not restricted by the reserved list.
        assert!(validate_name("ls").is_ok());
    }
}
