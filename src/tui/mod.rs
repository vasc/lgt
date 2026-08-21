mod app;
mod rows;
mod ui;
mod view;

use std::io::{IsTerminal, Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use ratatui::crossterm::event::{self, Event};

use crate::store::{Liveness, Store};
use app::App;
use view::StreamView;

const REFRESH_EVERY: Duration = Duration::from_millis(400);
const POLL_EVERY: Duration = Duration::from_millis(100);

pub fn run(store: &Store) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        bail!("the TUI needs a terminal on stdout");
    }
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, store);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, store: &Store) -> Result<()> {
    let mut app = App::new();
    let mut last_refresh: Option<Instant> = None;
    loop {
        let due = last_refresh.is_none_or(|t| t.elapsed() >= REFRESH_EVERY);
        if due {
            let mut streams = store.list().unwrap_or_default();
            // Non-finalized streams carry stale counters; show the data
            // file's real size so the list reflects live progress.
            for meta in &mut streams {
                if meta.liveness() != Liveness::Done {
                    meta.bytes = store.data_len(&meta.id);
                }
            }
            app.refresh(streams);
            last_refresh = Some(Instant::now());
        }
        sync_view(&mut app, store);
        terminal.draw(|frame| ui::render(frame, &mut app))?;
        if event::poll(POLL_EVERY)?
            && let Event::Key(key) = event::read()?
        {
            app.on_key(key);
        }
        if app.quit {
            return Ok(());
        }
    }
}

/// Keep the content view aligned with the selection and incrementally pull in
/// anything the capturing process appended since the last tick — this is what
/// makes watching a still-running stream live.
fn sync_view(app: &mut App, store: &Store) {
    if let Some(id) = app.selected_stream_id() {
        let switch = app.view.as_ref().map(|v| v.id != id).unwrap_or(true);
        if switch {
            let running = app
                .meta(&id)
                .map(|m| m.liveness() == Liveness::Running)
                .unwrap_or(false);
            app.view = Some(StreamView::new(id));
            app.scroll = 0;
            app.hscroll = 0;
            app.match_line = None;
            app.follow = running;
        }
    }
    if let Some(view) = &mut app.view {
        poll_append(store, view);
        if app.follow {
            app.scroll = view.total_rows().saturating_sub(app.content_height.max(1));
        }
    }
}

fn poll_append(store: &Store, view: &mut StreamView) {
    let len = store.data_len(&view.id);
    if len < view.offset {
        // The file shrank underneath us (removed/recreated): reload.
        view.reset();
    }
    if len == view.offset {
        return;
    }
    let Ok(mut file) = std::fs::File::open(store.data_path(&view.id)) else {
        return;
    };
    if file.seek(SeekFrom::Start(view.offset)).is_err() {
        return;
    }
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                view.append(&buf[..n]);
                view.offset += u64::try_from(n).unwrap_or(0);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Status, StreamMeta};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn buffer_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        let width = usize::from(buffer.area.width);
        let cells: Vec<String> = buffer
            .content()
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        cells.chunks(width).map(|row| row.concat()).collect()
    }

    #[test]
    fn renders_streams_and_live_content() {
        let mut app = App::new();
        app.refresh(vec![StreamMeta {
            id: "s-1".to_string(),
            name: Some("build".to_string()),
            group_id: "g-1".to_string(),
            group_name: Some("deploy".to_string()),
            pid: crate::proc::my_pid(),
            pgid: 1,
            started_at_ms: 1_000,
            ended_at_ms: None,
            status: Status::Running,
            bytes: 0,
            lines: 0,
            cwd: None,
            copy: None,
            pipeline: None,
        }]);
        let mut view = StreamView::new("s-1".to_string());
        view.append(b"compiling widget v0.1.0\nerror: oh no\n");
        app.view = Some(view);

        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &mut app)).unwrap();

        let rows = buffer_rows(&terminal);
        let all = rows.join("\n");
        assert!(all.contains("deploy"), "group label missing:\n{all}");
        assert!(all.contains("build"), "stream name missing:\n{all}");
        assert!(all.contains("compiling widget"), "content missing:\n{all}");
        assert!(all.contains("[running]"), "status missing:\n{all}");
    }

    #[test]
    fn poll_append_tails_a_growing_file() {
        use std::io::Write;
        let store = crate::store::testutil::temp_store();
        store.create_stream_dir("s-t").unwrap();
        let path = store.data_path("s-t");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"one\ntwo\n").unwrap();

        let mut view = StreamView::new("s-t".to_string());
        poll_append(&store, &mut view);
        assert_eq!(view.lines, vec!["one", "two"]);

        f.write_all(b"three\n").unwrap();
        poll_append(&store, &mut view);
        assert_eq!(view.lines, vec!["one", "two", "three"]);

        // Truncation triggers a clean reload.
        drop(f);
        std::fs::write(&path, b"fresh\n").unwrap();
        poll_append(&store, &mut view);
        assert_eq!(view.lines, vec!["fresh"]);
    }
}
