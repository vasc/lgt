//! Key handling for [`App`]: navigation, search, and proposing actions.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::{Action, App, Focus, Pending};
use crate::store::{Liveness, Shelf};
use crate::tui::rows::Row;
use crate::tui::view::StreamView;
use crate::units;

impl App {
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return None;
        }
        self.message = None;
        if let Some(pending) = self.pending.take() {
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                return Some(pending.action);
            }
            self.message = Some("cancelled".to_string());
            return None;
        }
        if self.search_input.is_some() {
            self.on_search_key(key);
            return None;
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
            KeyCode::Char('d') => self.propose(false),
            KeyCode::Char('a') => self.propose(true),
            _ => match self.focus {
                Focus::List => self.on_list_key(key),
                Focus::Content => self.on_content_key(key),
            },
        }
        None
    }

    /// Ask for confirmation to delete (or archive/restore) the selected
    /// stream, or every finished stream of the selected group.
    fn propose(&mut self, moving: bool) {
        let verb = match (moving, self.shelf) {
            (false, _) => "delete",
            (true, Shelf::Active) => "archive",
            (true, Shelf::Archive) => "restore",
        };
        let (ids, label) = match self.rows.get(self.selected) {
            Some(Row::Stream(id)) => {
                let Some(entry) = self.catalog.get(id) else {
                    return;
                };
                if entry.liveness == Liveness::Running {
                    self.message = Some(format!(
                        "{} is still running — finish it first, or `lgt rm -f` it",
                        entry.meta.display_name()
                    ));
                    return;
                }
                (vec![id.clone()], entry.meta.display_name().to_string())
            }
            Some(Row::Group(group)) => {
                let ids: Vec<String> = group
                    .members
                    .iter()
                    .filter(|id| {
                        self.catalog
                            .get(id)
                            .is_some_and(|e| e.liveness != Liveness::Running)
                    })
                    .cloned()
                    .collect();
                if ids.is_empty() {
                    self.message = Some("every stream in this group is still running".to_string());
                    return;
                }
                let skipped = group.members.len() - ids.len();
                let note = if skipped > 0 {
                    format!(", skipping {skipped} running")
                } else {
                    String::new()
                };
                let label = format!("{} finished streams of `{}`{note}", ids.len(), group.label);
                (ids, label)
            }
            None => return,
        };
        let size: u64 = ids
            .iter()
            .filter_map(|id| self.catalog.get(id))
            .map(|e| e.bytes)
            .sum();
        let prompt = format!("{verb} {label} ({})? y/n", units::human_bytes(size));
        let action = if moving {
            Action::Move(ids)
        } else {
            Action::Delete(ids)
        };
        self.pending = Some(Pending { action, prompt });
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
                self.match_line = None;
                if query.is_empty() {
                    self.search_query = None;
                } else {
                    self.search_query = Some(query);
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
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),
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
        let step = usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX);
        let next = if delta >= 0 {
            self.selected.saturating_add(step)
        } else {
            self.selected.saturating_sub(step)
        };
        self.set_selection(next);
    }

    pub fn set_selection(&mut self, idx: usize) {
        self.selected = idx.min(self.rows.len().saturating_sub(1));
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
        // Search the whole stream, not just the part indexed so far. On a
        // multi-GB stream this blocks once; the user asked for it.
        if let Some(view) = &mut self.view
            && !view.caught_up()
        {
            view.index_more(usize::MAX);
        }
        let Some(found) = self.find_match(forward) else {
            self.message = self
                .search_query
                .as_ref()
                .map(|q| format!("no match for `{q}`"));
            return;
        };
        self.match_line = Some(found);
        self.scroll = found.saturating_sub(self.content_height.max(1) / 3);
        self.follow = false;
    }

    fn find_match(&self, forward: bool) -> Option<usize> {
        let query = self.search_query.as_deref()?;
        let view = self.view.as_ref()?;
        let n = view.total_rows();
        if n == 0 {
            return None;
        }
        let anchor = self.match_line.unwrap_or_else(|| self.scroll.min(n - 1));
        if forward {
            view.find(anchor + 1..n, false, query)
                .or_else(|| view.find(0..anchor + 1, false, query))
        } else {
            view.find(0..anchor, true, query)
                .or_else(|| view.find(anchor..n, true, query))
        }
    }
}
