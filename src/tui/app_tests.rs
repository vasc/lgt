use super::*;
use crate::store::testutil::meta;
use crate::store::{Status, StreamMeta};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn grouped(id: &str, group: &str, started: i64) -> StreamMeta {
    let mut m = meta(id, Some(id), started);
    m.group_id = group.to_string();
    m.bytes = 10;
    m
}

fn running(mut m: StreamMeta) -> StreamMeta {
    m.status = Status::Running;
    m.pid = crate::proc::my_pid();
    m
}

fn app_with(metas: Vec<StreamMeta>) -> App {
    let mut app = App::new(Shelf::Active);
    app.catalog = Catalog::from_metas(metas);
    app.sync_rows();
    app
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn row_keys(app: &App) -> Vec<String> {
    app.rows.iter().map(App::row_key).collect()
}

#[test]
fn rows_group_and_order_streams() {
    let app = app_with(vec![
        grouped("s-2", "g-1", 2_000),
        grouped("s-1", "g-1", 1_000),
        grouped("s-9", "g-2", 9_000),
    ]);
    assert_eq!(
        row_keys(&app),
        vec!["group:g-2", "s-9", "group:g-1", "s-1", "s-2"]
    );
    assert_eq!(app.selected_stream_id().as_deref(), Some("s-9"));
}

#[test]
fn rows_are_only_rebuilt_when_the_catalog_generation_changes() {
    let mut app = app_with(vec![grouped("s-1", "g-1", 1_000)]);
    assert!(!app.sync_rows(), "same generation: no rebuild");
}

#[test]
fn collapse_hides_group_members() {
    let mut app = app_with(vec![
        grouped("s-1", "g-1", 1_000),
        grouped("s-2", "g-1", 2_000),
    ]);
    app.set_selection(0);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.rows.len(), 1);
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.rows.len(), 3);
}

#[test]
fn delete_asks_for_confirmation_then_yields_the_action() {
    let mut app = app_with(vec![grouped("s-1", "g-1", 1_000)]);
    assert_eq!(app.selected_stream_id().as_deref(), Some("s-1"));
    assert_eq!(app.on_key(key(KeyCode::Char('d'))), None);
    let prompt = &app.pending.as_ref().unwrap().prompt;
    assert!(prompt.starts_with("delete s-1"), "{prompt}");
    assert_eq!(
        app.on_key(key(KeyCode::Char('y'))),
        Some(Action::Delete(vec!["s-1".to_string()]))
    );
    assert!(app.pending.is_none());
}

#[test]
fn any_other_key_cancels_a_pending_action() {
    let mut app = app_with(vec![grouped("s-1", "g-1", 1_000)]);
    app.on_key(key(KeyCode::Char('a')));
    assert!(app.pending.as_ref().unwrap().prompt.starts_with("archive"));
    assert_eq!(app.on_key(key(KeyCode::Char('n'))), None);
    assert!(app.pending.is_none());
    assert_eq!(app.message.as_deref(), Some("cancelled"));
    assert!(!app.quit, "the cancelling key is consumed, not interpreted");
}

#[test]
fn group_actions_cover_only_finished_members() {
    let mut app = app_with(vec![
        grouped("s-1", "g-1", 1_000),
        running(grouped("s-2", "g-1", 2_000)),
        grouped("s-3", "g-1", 3_000),
    ]);
    app.set_selection(0);
    app.on_key(key(KeyCode::Char('a')));
    let prompt = app.pending.as_ref().unwrap().prompt.clone();
    assert!(prompt.contains("2 finished streams"), "{prompt}");
    assert!(prompt.contains("skipping 1 running"), "{prompt}");
    assert_eq!(
        app.on_key(key(KeyCode::Char('y'))),
        Some(Action::Move(vec!["s-1".to_string(), "s-3".to_string()]))
    );
}

#[test]
fn running_streams_cannot_be_deleted_from_the_tui() {
    let mut app = app_with(vec![running(grouped("s-1", "g-1", 1_000))]);
    app.on_key(key(KeyCode::Char('d')));
    assert!(app.pending.is_none());
    assert!(app.message.as_deref().unwrap().contains("still running"));
}

#[test]
fn archive_shelf_offers_restore() {
    let mut app = app_with(vec![grouped("s-1", "g-1", 1_000)]);
    app.shelf = Shelf::Archive;
    app.on_key(key(KeyCode::Char('a')));
    assert!(app.pending.as_ref().unwrap().prompt.starts_with("restore"));
}

fn view_with(content: &[u8]) -> StreamView {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "lgt-app-view-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(&path, content).unwrap();
    let mut view = StreamView::new("s".to_string(), path);
    view.index_more(usize::MAX);
    view
}

#[test]
fn search_jumps_and_wraps() {
    let mut app = App::new(Shelf::Active);
    app.view = Some(view_with(b"alpha\nbeta\ngamma\nBETA again\n"));
    app.content_height = 10;
    app.search_query = Some("beta".to_string());
    app.jump_match(true);
    assert_eq!(app.match_line, Some(1));
    app.jump_match(true);
    assert_eq!(app.match_line, Some(3));
    app.jump_match(true);
    assert_eq!(app.match_line, Some(1));
    app.jump_match(false);
    assert_eq!(app.match_line, Some(3));
}

#[test]
fn search_covers_parts_of_the_stream_not_yet_indexed() {
    let mut content = b"x\n".repeat(1000);
    content.extend_from_slice(b"needle at the end\n");
    let path = std::env::temp_dir().join(format!("lgt-app-partial-{}", std::process::id()));
    std::fs::write(&path, &content).unwrap();
    let mut view = StreamView::new("s".to_string(), path);
    view.index_more(64);
    assert!(!view.caught_up());
    let mut app = App::new(Shelf::Active);
    app.view = Some(view);
    app.content_height = 10;
    app.search_query = Some("needle".to_string());
    app.jump_match(true);
    assert_eq!(app.match_line, Some(1000));
}

#[test]
fn follow_disabled_by_scrolling_up() {
    let mut app = App::new(Shelf::Active);
    app.view = Some(view_with(b"a\nb\nc\n"));
    app.focus = Focus::Content;
    app.follow = true;
    app.scroll = 2;
    app.on_key(key(KeyCode::Char('k')));
    assert!(!app.follow);
    assert_eq!(app.scroll, 1);
    app.on_key(key(KeyCode::Char('G')));
    assert!(app.follow);
}
