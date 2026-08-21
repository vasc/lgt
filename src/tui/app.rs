use std::collections::HashSet;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;

use crate::store::StreamMeta;
use crate::tui::rows::{Row, build_rows};
use crate::tui::view::StreamView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Content,
}

pub struct App {
    pub streams: Vec<StreamMeta>,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub list_state: ListState,
    pub collapsed: HashSet<String>,
    pub focus: Focus,
    pub view: Option<StreamView>,
    pub follow: bool,
    pub scroll: usize,
    pub hscroll: usize,
    pub content_height: usize,
    pub search_query: Option<String>,
    pub search_input: Option<String>,
    pub match_line: Option<usize>,
    pub quit: bool,
    initialized: bool,
}

impl App {
    pub fn new() -> Self {
        App {
            streams: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            list_state: ListState::default(),
            collapsed: HashSet::new(),
            focus: Focus::List,
            view: None,
            follow: false,
            scroll: 0,
            hscroll: 0,
            content_height: 1,
            search_query: None,
            search_input: None,
            match_line: None,
            quit: false,
            initialized: false,
        }
    }

    pub fn meta(&self, id: &str) -> Option<&StreamMeta> {
        self.streams.iter().find(|m| m.id == id)
    }

    pub fn selected_stream_id(&self) -> Option<String> {
        match self.rows.get(self.selected) {
            Some(Row::Stream(id)) => Some(id.clone()),
            _ => None,
        }
    }

    /// Rebuild the row tree from a fresh store snapshot, keeping the current
    /// selection where possible.
    pub fn refresh(&mut self, streams: Vec<StreamMeta>) {
        let prior = match self.rows.get(self.selected) {
            Some(Row::Stream(id)) => Some(id.clone()),
            Some(Row::Group(g)) => Some(format!("group:{}", g.gid)),
            None => None,
        };
        self.streams = streams;
        self.rows = build_rows(&self.streams, &self.collapsed);
        let restored = prior.and_then(|key| {
            self.rows.iter().position(|row| match row {
                Row::Stream(id) => *id == key,
                Row::Group(g) => format!("group:{}", g.gid) == key,
            })
        });
        self.selected = match restored {
            Some(idx) => idx,
            None => self.selected.min(self.rows.len().saturating_sub(1)),
        };
        if !self.initialized && !self.rows.is_empty() {
            self.initialized = true;
            // Open on the most recently started stream.
            let latest = self
                .streams
                .iter()
                .max_by_key(|m| m.started_at_ms)
                .map(|m| m.id.clone());
            if let Some(latest) = latest
                && let Some(idx) = self
                    .rows
                    .iter()
                    .position(|row| matches!(row, Row::Stream(id) if *id == latest))
            {
                self.selected = idx;
            }
        }
        self.list_state.select(Some(self.selected));
    }

    fn rebuild_rows(&mut self) {
        self.rows = build_rows(&self.streams, &self.collapsed);
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        self.list_state.select(Some(self.selected));
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.search_input.is_some() {
            self.on_search_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::List => Focus::Content,
                    Focus::Content => Focus::List,
                };
            }
            KeyCode::Char('/') => {
                self.search_input = Some(String::new());
                self.focus = Focus::Content;
            }
            KeyCode::Char('f') => self.follow = !self.follow,
            _ => match self.focus {
                Focus::List => self.on_list_key(key),
                Focus::Content => self.on_content_key(key),
            },
        }
    }

    fn on_search_key(&mut self, key: KeyEvent) {
        let Some(input) = &mut self.search_input else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.search_input = None,
            KeyCode::Enter => {
                let query = input.clone();
                self.search_input = None;
                if query.is_empty() {
                    self.search_query = None;
                    self.match_line = None;
                } else {
                    self.search_query = Some(query);
                    self.match_line = None;
                    self.jump_match(true);
                }
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(c) => input.push(c),
            _ => {}
        }
    }

    fn on_list_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => self.set_selection(0),
            KeyCode::Char('G') | KeyCode::End => {
                self.set_selection(self.rows.len().saturating_sub(1));
            }
            KeyCode::Enter | KeyCode::Char(' ') => match self.rows.get(self.selected) {
                Some(Row::Group(g)) => {
                    let gid = g.gid.clone();
                    if !self.collapsed.remove(&gid) {
                        self.collapsed.insert(gid);
                    }
                    self.rebuild_rows();
                }
                Some(Row::Stream(_)) => self.focus = Focus::Content,
                None => {}
            },
            _ => {}
        }
    }

    pub fn move_selection(&mut self, delta: i64) {
        if self.rows.is_empty() {
            return;
        }
        let next = if delta >= 0 {
            self.selected
                .saturating_add(usize::try_from(delta).unwrap_or(0))
        } else {
            self.selected
                .saturating_sub(usize::try_from(delta.saturating_neg()).unwrap_or(0))
        };
        self.set_selection(next);
    }

    pub fn set_selection(&mut self, idx: usize) {
        self.selected = idx.min(self.rows.len().saturating_sub(1));
        self.list_state.select(Some(self.selected));
    }

    fn on_content_key(&mut self, key: KeyEvent) {
        let total = self.view.as_ref().map(StreamView::total_rows).unwrap_or(0);
        let page = self.content_height.max(1);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.scroll = (self.scroll + 1).min(total.saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                self.follow = false;
            }
            KeyCode::PageDown => self.scroll = (self.scroll + page).min(total.saturating_sub(1)),
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(page);
                self.follow = false;
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.scroll = 0;
                self.follow = false;
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.scroll = total.saturating_sub(page);
                self.follow = true;
            }
            KeyCode::Left => self.hscroll = self.hscroll.saturating_sub(8),
            KeyCode::Right => self.hscroll = self.hscroll.saturating_add(8),
            KeyCode::Char('n') => self.jump_match(true),
            KeyCode::Char('N') => self.jump_match(false),
            _ => {}
        }
    }

    pub fn jump_match(&mut self, forward: bool) {
        let Some(found) = self.find_match(forward) else {
            return;
        };
        self.match_line = Some(found);
        self.scroll = found.saturating_sub(self.content_height.max(1) / 3);
        self.follow = false;
    }

    fn find_match(&self, forward: bool) -> Option<usize> {
        let query = self.search_query.as_ref()?.to_ascii_lowercase();
        let view = self.view.as_ref()?;
        let n = view.lines.len();
        if n == 0 {
            return None;
        }
        let anchor = self.match_line.unwrap_or_else(|| self.scroll.min(n - 1));
        let matches = |i: &usize| view.lines[*i].to_ascii_lowercase().contains(&query);
        if forward {
            (anchor + 1..n)
                .find(matches)
                .or_else(|| (0..=anchor.min(n - 1)).find(matches))
        } else {
            (0..anchor)
                .rev()
                .find(matches)
                .or_else(|| (anchor..n).rev().find(matches))
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::store::Status;
    use ratatui::crossterm::event::KeyEvent;

    pub fn fake_meta(id: &str, name: Option<&str>, group_id: &str, started: i64) -> StreamMeta {
        StreamMeta {
            id: id.to_string(),
            name: name.map(str::to_string),
            group_id: group_id.to_string(),
            group_name: None,
            pid: 1,
            pgid: 1,
            started_at_ms: started,
            ended_at_ms: Some(started),
            status: Status::Done,
            bytes: 10,
            lines: 2,
            cwd: None,
            copy: None,
            pipeline: None,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn rows_group_and_order_streams() {
        let mut app = App::new();
        app.refresh(vec![
            fake_meta("s-2", Some("second"), "g-1", 2_000),
            fake_meta("s-1", Some("first"), "g-1", 1_000),
            fake_meta("s-9", None, "g-2", 9_000),
        ]);
        // Newest group first, then its streams in pipeline order.
        let kinds: Vec<String> = app
            .rows
            .iter()
            .map(|r| match r {
                Row::Group(g) => format!("group:{}", g.gid),
                Row::Stream(id) => format!("stream:{id}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "group:g-2",
                "stream:s-9",
                "group:g-1",
                "stream:s-1",
                "stream:s-2"
            ]
        );
        // Initial selection is the most recent stream.
        assert_eq!(app.selected_stream_id().as_deref(), Some("s-9"));
    }

    #[test]
    fn collapse_hides_group_members() {
        let mut app = App::new();
        app.refresh(vec![
            fake_meta("s-1", None, "g-1", 1_000),
            fake_meta("s-2", None, "g-1", 2_000),
        ]);
        app.set_selection(0);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.rows.len(), 1);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.rows.len(), 3);
    }

    #[test]
    fn search_jumps_and_wraps() {
        let mut app = App::new();
        let mut view = StreamView::new("s".to_string());
        view.append(b"alpha\nbeta\ngamma\nBETA again\n");
        app.view = Some(view);
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
    fn follow_disabled_by_scrolling_up() {
        let mut app = App::new();
        let mut view = StreamView::new("s".to_string());
        view.append(b"a\nb\nc\n");
        app.view = Some(view);
        app.focus = Focus::Content;
        app.follow = true;
        app.scroll = 2;
        app.on_key(key(KeyCode::Char('k')));
        assert!(!app.follow);
        assert_eq!(app.scroll, 1);
        app.on_key(key(KeyCode::Char('G')));
        assert!(app.follow);
    }
}
