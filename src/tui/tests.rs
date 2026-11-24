use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use super::actions::{load, perform, refresh_catalog, sync_view, update_advice};
use super::app::{Action, App};
use super::catalog::Catalog;
use super::{Batch, handle_events, ui};
use crate::store::testutil::{meta, put, temp_store};
use crate::store::{Shelf, Status};

fn screen(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let width = usize::from(buffer.area.width);
    let cells: Vec<String> = buffer
        .content()
        .iter()
        .map(|c| c.symbol().to_string())
        .collect();
    cells
        .chunks(width)
        .map(|row| row.concat())
        .collect::<Vec<_>>()
        .join("\n")
}

fn draw(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| ui::render(frame, app)).unwrap();
    screen(&terminal)
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn renders_streams_and_live_content() {
    let store = temp_store();
    let mut live = meta("s-1", Some("build"), 1_000);
    live.group_name = Some("deploy".to_string());
    live.status = Status::Running;
    live.pid = crate::proc::my_pid();
    put(&store, &live, b"compiling widget v0.1.0\nerror: oh no\n");

    let mut app = App::new(Shelf::Active);
    load(&mut app, &store);
    sync_view(&mut app, &store);
    let all = draw(&mut app, 100, 20);
    assert!(all.contains("deploy"), "group label missing:\n{all}");
    assert!(all.contains("build"), "stream name missing:\n{all}");
    assert!(all.contains("compiling widget"), "content missing:\n{all}");
    assert!(all.contains("[running]"), "status missing:\n{all}");
    assert!(all.contains("streams (1)"), "count missing:\n{all}");
}

#[test]
fn list_window_follows_a_selection_far_down() {
    let store = temp_store();
    for i in 0..500 {
        let mut m = meta(&format!("s-{i:04}"), Some(&format!("job{i:04}")), i);
        m.group_id = format!("g-{i:04}");
        put(&store, &m, b"x\n");
    }
    let mut app = App::new(Shelf::Active);
    load(&mut app, &store);
    // Newest first: the last row is the oldest stream.
    app.set_selection(app.rows.len() - 1);
    let all = draw(&mut app, 100, 20);
    assert!(
        all.contains("job0000"),
        "selected row must be on screen:\n{all}"
    );
    assert!(
        !all.contains("job0499"),
        "top of the list scrolled away:\n{all}"
    );
    app.set_selection(0);
    let all = draw(&mut app, 100, 20);
    assert!(all.contains("job0499"), "{all}");
}

#[test]
fn drawing_cost_does_not_grow_with_history() {
    // The old renderer looked every row up linearly (O(n²) per frame) and
    // probed every pid; 20k streams took seconds per frame that way.
    let metas = (0..20_000)
        .map(|i| {
            let mut m = meta(&format!("s-{i:05}"), Some("job"), i);
            m.group_id = format!("g-{i:05}");
            m
        })
        .collect();
    let mut app = App::new(Shelf::Active);
    app.catalog = Catalog::from_metas(metas);
    app.sync_rows();
    app.set_selection(10_000);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let started = Instant::now();
    for _ in 0..10 {
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();
    }
    let per_frame = started.elapsed() / 10;
    // A windowed frame takes a few ms; the quadratic one took well over a
    // second here. The wide margin keeps a loaded CI machine from flaking.
    assert!(
        per_frame < Duration::from_millis(250),
        "a frame took {per_frame:?} with 20k streams"
    );
}

#[test]
fn queued_keys_are_handled_in_one_batch() {
    let store = temp_store();
    for i in 0..300 {
        let mut m = meta(&format!("s-{i:03}"), None, i);
        m.group_id = format!("g-{i:03}");
        put(&store, &m, b"");
    }
    let mut app = App::new(Shelf::Active);
    load(&mut app, &store);
    app.set_selection(0);
    let mut queue: VecDeque<Event> = (0..200).map(|_| key(KeyCode::Char('j'))).collect();
    let batch = handle_events(&mut app, &store, || Ok(queue.pop_front())).unwrap();
    assert!(matches!(batch, Batch::Handled { acted: false }));
    assert!(
        queue.is_empty(),
        "every queued key is consumed before drawing"
    );
    assert_eq!(app.selected, 200);
}

#[test]
fn a_quit_key_stops_the_batch() {
    let store = temp_store();
    let mut app = App::new(Shelf::Active);
    let mut queue: VecDeque<Event> = [key(KeyCode::Char('q')), key(KeyCode::Char('j'))].into();
    let batch = handle_events(&mut app, &store, || Ok(queue.pop_front())).unwrap();
    assert!(matches!(batch, Batch::Quit));
}

#[test]
fn advice_banner_is_drawn_at_the_top_and_wraps() {
    let mut app = App::new(Shelf::Active);
    app.advice = Some(format!("⚠ slow: test banner {}", "word ".repeat(30)));
    let all = draw(&mut app, 60, 12);
    let mut lines = all.lines();
    assert!(lines.next().unwrap().contains("slow: test banner"), "{all}");
    assert!(lines.next().unwrap().contains("word"), "wrapped:\n{all}");
}

#[test]
fn advice_changes_are_reported_so_the_frame_is_redrawn() {
    let store = temp_store();
    for i in 0..10 {
        put(&store, &meta(&format!("s-{i}"), None, i), b"");
    }
    let mut app = App::new(Shelf::Active);
    load(&mut app, &store);
    assert!(app.advice.is_none());
    for _ in 0..10 {
        app.timings.refresh.record(Duration::from_millis(500));
    }
    assert!(update_advice(&mut app), "a new warning needs a redraw");
    assert!(app.advice.is_some());
    assert!(
        !update_advice(&mut app),
        "unchanged warning: nothing to redraw"
    );
    assert!(
        refresh_catalog(&mut app, &store),
        "the warning clearing is a change too"
    );
}

#[test]
fn follow_tracks_a_growing_stream() {
    use std::io::Write;
    let store = temp_store();
    let mut live = meta("s-live", None, 1_000);
    live.status = Status::Running;
    live.pid = crate::proc::my_pid();
    put(&store, &live, b"");
    let mut app = App::new(Shelf::Active);
    load(&mut app, &store);
    app.content_height = 3;
    sync_view(&mut app, &store);
    assert!(app.follow, "running streams open in follow mode");

    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(store.data_path("s-live"))
        .unwrap();
    for i in 0..10 {
        writeln!(f, "line {i}").unwrap();
    }
    assert!(sync_view(&mut app, &store));
    assert_eq!(app.scroll, 7, "10 lines in a 3-line pane: bottom at 7");
}

#[test]
fn perform_archives_and_deletes() {
    let store = temp_store();
    put(&store, &meta("s-1", None, 1), b"a");
    put(&store, &meta("s-2", None, 2), b"b");
    let mut app = App::new(Shelf::Active);
    load(&mut app, &store);

    perform(&mut app, &store, Action::Move(vec!["s-1".to_string()]));
    assert_eq!(app.message.as_deref(), Some("archived 1 stream(s)"));
    assert!(app.catalog.get("s-1").is_none());
    assert!(store.on(Shelf::Archive).meta_path("s-1").exists());

    perform(&mut app, &store, Action::Delete(vec!["s-2".to_string()]));
    assert_eq!(app.message.as_deref(), Some("deleted 1 stream(s)"));
    assert_eq!(app.catalog.len(), 0);
    assert!(!store.stream_dir("s-2").exists());
}

#[test]
fn perform_reports_every_failure() {
    let store = temp_store();
    put(&store, &meta("s-1", None, 1), b"a");
    let mut app = App::new(Shelf::Active);
    load(&mut app, &store);
    let ids = vec![
        "gone-1".to_string(),
        "s-1".to_string(),
        "gone-2".to_string(),
    ];
    perform(&mut app, &store, Action::Delete(ids));
    let message = app.message.unwrap();
    assert!(
        message.starts_with("deleted 1 stream(s); 2 failed"),
        "{message}"
    );
}
