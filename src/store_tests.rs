use super::testutil::{meta, put, temp_store};
use super::*;

fn dead(id: &str) -> StreamMeta {
    let mut m = meta(id, None, 1_000);
    m.status = Status::Running;
    m.ended_at_ms = None;
    m.pid = -1;
    m
}

#[test]
fn meta_roundtrip() {
    let store = temp_store();
    let m = meta("s-1", Some("build"), 1_000);
    put(&store, &m, b"");
    let back = store.read_meta(&m.id).unwrap();
    assert_eq!(back.id, m.id);
    assert_eq!(back.name.as_deref(), Some("build"));
    assert_eq!(back.status, Status::Done);
}

#[test]
fn metas_without_machine_or_copy_still_parse() {
    let store = temp_store();
    store.create_stream_dir("s-old").unwrap();
    let mut json = serde_json::to_value(meta("s-old", None, 1)).unwrap();
    let object = json.as_object_mut().unwrap();
    object.remove("machine");
    object.remove("copy");
    std::fs::write(store.meta_path("s-old"), serde_json::to_vec(&json).unwrap()).unwrap();
    let back = store.read_meta("s-old").unwrap();
    assert_eq!(back.machine, None);
    assert_eq!(back.copy, None);
}

#[test]
fn resolve_prefers_exact_id_then_latest_name() {
    let store = temp_store();
    let older = meta("s-old", Some("x"), 1_000);
    let newer = meta("s-new", Some("x"), 2_000);
    put(&store, &older, b"");
    put(&store, &newer, b"");
    assert_eq!(store.resolve("x").unwrap().id, newer.id);
    assert_eq!(store.resolve(&older.id).unwrap().id, older.id);
    assert!(store.resolve("missing").is_err());
}

#[test]
fn dead_running_stream_reports_died() {
    let mut m = dead("s-1");
    assert_eq!(m.liveness(), Liveness::Died);
    m.pid = crate::proc::my_pid();
    assert_eq!(m.liveness(), Liveness::Running);
    m.status = Status::Died;
    assert_eq!(m.liveness(), Liveness::Died, "persisted died is final");
}

#[test]
fn captures_from_another_machine_are_never_declared_dead() {
    let mut m = dead("s-1");
    m.machine = Some("0000-some-other-machine".to_string());
    assert!(m.is_foreign());
    assert_eq!(m.liveness(), Liveness::Running);
    m.machine = crate::proc::machine_id().map(str::to_string);
    assert!(!m.is_foreign());
    assert_eq!(m.liveness(), Liveness::Died);
}

#[test]
fn captures_from_another_pid_namespace_are_never_declared_dead() {
    // Same machine (or no machine id at all), different container.
    let mut m = dead("s-1");
    m.pid_ns = Some("pid:[1]".to_string());
    let ours_known = crate::proc::pid_namespace().is_some();
    assert_eq!(
        m.is_foreign(),
        ours_known && crate::proc::pid_namespace() != Some("pid:[1]")
    );
    m.pid_ns = crate::proc::pid_namespace().map(str::to_string);
    assert!(!m.is_foreign());
}

#[test]
fn last_activity_is_the_end_or_the_last_write() {
    let store = temp_store();
    let mut m = meta("s-1", None, 1_000);
    m.ended_at_ms = Some(5_000);
    assert_eq!(store.last_activity_ms(&m), 5_000);
    m.ended_at_ms = None;
    assert_eq!(store.last_activity_ms(&m), 1_000);

    // Running: when its data file was last written.
    m.status = Status::Running;
    assert_eq!(store.last_activity_ms(&m), 1_000, "no data file yet");
    put(&store, &m, b"x");
    let written = system_time_ms(
        std::fs::metadata(store.data_path("s-1"))
            .unwrap()
            .modified()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(store.last_activity_ms(&m), written);
}

#[test]
fn observe_reports_dead_captures_without_writing() {
    let store = temp_store();
    put(&store, &dead("s-dead"), b"one\ntwo\nthr");
    let before = std::fs::read(store.meta_path("s-dead")).unwrap();

    let observed = store.observe(dead("s-dead"));
    assert!(observed.died_unrecorded);
    assert_eq!(observed.meta.status, Status::Died);
    assert_eq!(observed.meta.bytes, 11);
    assert!(observed.meta.ended_at_ms.is_some());
    assert_eq!(std::fs::read(store.meta_path("s-dead")).unwrap(), before);
}

#[test]
fn record_death_writes_true_counts() {
    let store = temp_store();
    put(&store, &dead("s-dead"), b"one\ntwo\nthr");
    let recorded = store.record_death("s-dead").unwrap();
    assert_eq!(recorded.lines, 2);
    let on_disk = store.read_meta("s-dead").unwrap();
    assert_eq!(on_disk.status, Status::Died);
    assert_eq!(on_disk.bytes, 11);
    assert_eq!(on_disk.lines, 2);
    assert!(on_disk.ended_at_ms.is_some());
    // Idempotent: a second call finds nothing to do.
    let stamp = store.meta_stamp("s-dead");
    assert!(store.record_death("s-dead").is_none());
    assert_eq!(store.meta_stamp("s-dead"), stamp);
}

#[test]
fn observe_prefers_a_final_meta_written_after_our_read() {
    let store = temp_store();
    let stale = dead("s-raced");
    let mut finished = stale.clone();
    finished.status = Status::Done;
    finished.bytes = 42;
    put(&store, &finished, b"");

    let observed = store.observe(stale);
    assert!(!observed.died_unrecorded);
    assert_eq!(observed.meta.status, Status::Done);
    assert_eq!(observed.meta.bytes, 42);
    assert!(store.record_death("s-raced").is_none());
    assert_eq!(store.read_meta("s-raced").unwrap().status, Status::Done);
}

#[test]
fn observe_leaves_live_and_final_streams_alone() {
    let store = temp_store();
    let mut live = meta("s-live", None, 1_000);
    live.status = Status::Running;
    live.pid = crate::proc::my_pid();
    put(&store, &live, b"x\n");
    assert_eq!(store.observe(live).meta.status, Status::Running);
    assert!(store.record_death("s-live").is_none());
    assert_eq!(store.read_meta("s-live").unwrap().status, Status::Running);
    assert_eq!(
        store.observe(meta("s-done", None, 1_000)).meta.status,
        Status::Done
    );
}

#[test]
fn concurrent_meta_writers_never_collide() {
    let store = temp_store();
    let m = meta("s-1", None, 1_000);
    put(&store, &m, b"");
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..50 {
                    store.write_meta(&m).unwrap();
                }
            });
        }
    });
    let leftovers: Vec<_> = std::fs::read_dir(store.stream_dir("s-1"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "no temp files may be left behind");
}

#[test]
fn opening_does_not_create_the_archive() {
    let store = temp_store();
    assert!(!store.on(Shelf::Archive).streams_dir().exists());
    assert!(store.on(Shelf::Archive).list().unwrap().is_empty());
}

#[test]
fn shelves_are_isolated_and_moves_are_reversible() {
    let store = temp_store();
    let archive = store.on(Shelf::Archive);
    put(&store, &meta("s-1", Some("a"), 1_000), b"data");
    put(&store, &meta("s-2", Some("b"), 2_000), b"data");

    store.move_stream("s-1", &archive).unwrap();
    let active: Vec<String> = store.list().unwrap().into_iter().map(|m| m.id).collect();
    let archived: Vec<String> = archive.list().unwrap().into_iter().map(|m| m.id).collect();
    assert_eq!(active, vec!["s-2"]);
    assert_eq!(archived, vec!["s-1"]);
    assert!(
        store.resolve("a").is_err(),
        "archived streams leave the active shelf"
    );
    assert_eq!(archive.resolve("a").unwrap().id, "s-1");

    archive.move_stream("s-1", &store).unwrap();
    assert!(archive.list().unwrap().is_empty());
    assert_eq!(store.read_meta("s-1").unwrap().name.as_deref(), Some("a"));
}

#[test]
fn move_refuses_to_overwrite() {
    let store = temp_store();
    let archive = store.on(Shelf::Archive);
    put(&store, &meta("s-1", None, 1_000), b"active copy");
    put(&archive, &meta("s-1", None, 1_000), b"archived copy");
    assert!(store.move_stream("s-1", &archive).is_err());
    assert_eq!(
        std::fs::read(archive.data_path("s-1")).unwrap(),
        b"archived copy"
    );
}

#[test]
fn stream_size_counts_every_file() {
    let store = temp_store();
    let m = meta("s-1", None, 1_000);
    put(&store, &m, &[b'x'; 100]);
    let meta_len = std::fs::metadata(store.meta_path("s-1")).unwrap().len();
    assert_eq!(store.stream_size("s-1"), 100 + meta_len);
}
