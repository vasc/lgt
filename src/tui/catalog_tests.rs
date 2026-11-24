use super::*;
use crate::store::Status;
use crate::store::testutil::{meta, put, temp_store};

#[test]
fn finished_metas_are_read_once() {
    let store = temp_store();
    for i in 0..50 {
        put(&store, &meta(&format!("s-{i:02}"), None, i), b"x\n");
    }
    let mut catalog = Catalog::default();
    assert!(catalog.refresh(&store));
    assert_eq!(catalog.len(), 50);
    assert_eq!(catalog.meta_reads(), 50);
    for _ in 0..5 {
        assert!(!catalog.refresh(&store), "nothing changed");
    }
    assert_eq!(catalog.meta_reads(), 50, "finished metas are never re-read");
}

#[test]
fn large_initial_loads_take_the_parallel_path_and_load_everything() {
    let store = temp_store();
    let count = PARALLEL_LOAD_MIN * 3 + 7;
    for i in 0..count {
        let id = format!("s-{i:04}");
        put(
            &store,
            &meta(&id, Some("n"), i64::try_from(i).unwrap()),
            b"",
        );
    }
    // One unpublished directory mixed in must not break a chunk.
    store.create_stream_dir("s-unpublished").unwrap();
    let mut catalog = Catalog::default();
    assert!(catalog.refresh(&store));
    assert_eq!(catalog.len(), count);
    assert_eq!(catalog.meta_reads(), u64::try_from(count).unwrap());
    assert!(catalog.get("s-0000").is_some());
    assert!(catalog.get(&format!("s-{:04}", count - 1)).is_some());
}

#[test]
fn running_streams_are_re_read_only_when_their_meta_changes() {
    let store = temp_store();
    let mut live = meta("s-live", None, 1);
    live.status = Status::Running;
    live.pid = crate::proc::my_pid();
    put(&store, &live, b"abc");
    let mut catalog = Catalog::default();
    catalog.refresh(&store);
    assert_eq!(catalog.meta_reads(), 1);
    assert_eq!(catalog.get("s-live").unwrap().bytes, 3);

    // Data growth is reflected without re-reading meta or a new generation.
    std::fs::write(store.data_path("s-live"), b"abcdef").unwrap();
    let generation = catalog.generation();
    assert!(!catalog.refresh(&store));
    assert_eq!(catalog.meta_reads(), 1);
    assert_eq!(catalog.generation(), generation);
    assert_eq!(catalog.get("s-live").unwrap().bytes, 6);

    // Finalization rewrites the meta (a rename, so a new inode even when
    // length and mtime happen to match): re-read, new generation.
    live.status = Status::Done;
    live.bytes = 6;
    store.write_meta(&live).unwrap();
    assert!(catalog.refresh(&store));
    assert_eq!(catalog.get("s-live").unwrap().liveness, Liveness::Done);
}

#[test]
fn additions_and_removals_bump_the_generation() {
    let store = temp_store();
    put(&store, &meta("s-1", None, 1), b"");
    let mut catalog = Catalog::default();
    catalog.refresh(&store);
    let g1 = catalog.generation();

    put(&store, &meta("s-2", None, 2), b"");
    assert!(catalog.refresh(&store));
    assert!(catalog.generation() > g1);
    assert_eq!(catalog.len(), 2);

    store.remove_stream("s-1").unwrap();
    assert!(catalog.refresh(&store));
    assert!(catalog.get("s-1").is_none());
}

#[test]
fn unpublished_streams_are_skipped_until_their_meta_appears() {
    let store = temp_store();
    store.create_stream_dir("s-new").unwrap();
    let mut catalog = Catalog::default();
    catalog.refresh(&store);
    assert_eq!(catalog.len(), 0);
    store.write_meta(&meta("s-new", None, 1)).unwrap();
    assert!(catalog.refresh(&store));
    assert_eq!(catalog.len(), 1);
}

#[test]
fn dead_captures_show_as_died_at_once_and_are_recorded_in_the_background() {
    let store = temp_store();
    let mut dead = meta("s-dead", None, 1);
    dead.status = Status::Running;
    dead.pid = -1;
    put(&store, &dead, b"a\nb\n");
    let mut catalog = Catalog::default();
    catalog.refresh(&store);
    let entry = catalog.get("s-dead").unwrap();
    assert_eq!(entry.liveness, Liveness::Died);
    assert_eq!(entry.bytes, 4);

    catalog.wait_for_recorders();
    let on_disk = store.read_meta("s-dead").unwrap();
    assert_eq!(on_disk.status, Status::Died);
    assert_eq!(on_disk.lines, 2);
}

#[test]
fn a_running_stream_dying_is_noticed_on_refresh() {
    let store = temp_store();
    let mut live = meta("s-live", None, 1);
    live.status = Status::Running;
    live.pid = crate::proc::my_pid();
    put(&store, &live, b"x\n");
    let mut catalog = Catalog::default();
    catalog.refresh(&store);
    assert_eq!(catalog.get("s-live").unwrap().liveness, Liveness::Running);

    // Same meta, but now pointing at a pid that does not exist.
    live.pid = -1;
    store.write_meta(&live).unwrap();
    assert!(catalog.refresh(&store));
    assert_eq!(catalog.get("s-live").unwrap().liveness, Liveness::Died);
    catalog.wait_for_recorders();
    assert_eq!(store.read_meta("s-live").unwrap().status, Status::Died);
}
