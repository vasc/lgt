use super::{Status, Store, StreamMeta};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn temp_store() -> Store {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lgt-unit-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Store::open(dir).expect("temp store")
}

pub fn meta(id: &str, name: Option<&str>, started_at_ms: i64) -> StreamMeta {
    StreamMeta {
        id: id.to_string(),
        name: name.map(str::to_string),
        group_id: "g-test".to_string(),
        group_name: None,
        pid: 1,
        pgid: 1,
        machine: None,
        pid_ns: None,
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

/// Write `meta` plus a data file holding `data` into `store`.
pub fn put(store: &Store, meta: &StreamMeta, data: &[u8]) {
    store.create_stream_dir(&meta.id).unwrap();
    std::fs::write(store.data_path(&meta.id), data).unwrap();
    store.write_meta(meta).unwrap();
}
