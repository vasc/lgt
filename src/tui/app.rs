use std::collections::HashSet;

use crate::store::Shelf;
use crate::tui::catalog::Catalog;
use crate::tui::perf::Timings;
use crate::tui::rows::{Row, build_rows};
use crate::tui::view::StreamView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Content,
}

/// A store change requested from the TUI, executed by the event loop once
/// the user confirms it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Delete(Vec<String>),
    /// Archive (from the active shelf) or restore (from the archive).
    Move(Vec<String>),
}

pub struct Pending {
    pub action: Action,
    pub prompt: String,
}

pub struct App {
    pub shelf: Shelf,
    pub catalog: Catalog,
    pub rows: Vec<Row>,
    rows_generation: Option<u64>,
    pub selected: usize,
    /// First list row on screen; maintained by the renderer.
    pub list_offset: usize,
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
    pub pending: Option<Pending>,
    pub message: Option<String>,
    pub timings: Timings,
    pub advice: Option<String>,
    pub quit: bool,
}

impl App {
    pub fn new(shelf: Shelf) -> Self {
        App {
            shelf,
            catalog: Catalog::default(),
            rows: Vec::new(),
            rows_generation: None,
            selected: 0,
            list_offset: 0,
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
            pending: None,
            message: None,
            timings: Timings::default(),
            advice: None,
            quit: false,
        }
    }

    pub fn selected_stream_id(&self) -> Option<String> {
        match self.rows.get(self.selected) {
            Some(Row::Stream(id)) => Some(id.clone()),
            _ => None,
        }
    }

    fn row_key(row: &Row) -> String {
        match row {
            Row::Stream(id) => id.clone(),
            Row::Group(g) => format!("group:{}", g.gid),
        }
    }

    /// Rebuild the row tree when the catalog changed, keeping the selection
    /// on the same stream or group where possible. Returns true if rebuilt.
    pub fn sync_rows(&mut self) -> bool {
        let generation = self.catalog.generation();
        if self.rows_generation == Some(generation) {
            return false;
        }
        let first_build = self.rows_generation.is_none() || self.rows.is_empty();
        let prior = self.rows.get(self.selected).map(Self::row_key);
        self.rows = build_rows(&self.catalog, &self.collapsed);
        self.rows_generation = Some(generation);
        let restored =
            prior.and_then(|key| self.rows.iter().position(|row| Self::row_key(row) == key));
        self.selected = match restored {
            Some(idx) => idx,
            None => self.selected.min(self.rows.len().saturating_sub(1)),
        };
        if first_build {
            // Open on the most recently started stream.
            let latest = self
                .catalog
                .entries()
                .max_by_key(|e| e.meta.started_at_ms)
                .map(|e| e.meta.id.clone());
            if let Some(latest) = latest
                && let Some(idx) = self
                    .rows
                    .iter()
                    .position(|row| matches!(row, Row::Stream(id) if *id == latest))
            {
                self.selected = idx;
            }
        }
        true
    }

    fn rebuild_rows(&mut self) {
        let generation = self.catalog.generation();
        let prior = self.rows.get(self.selected).map(Self::row_key);
        self.rows = build_rows(&self.catalog, &self.collapsed);
        self.rows_generation = Some(generation);
        if let Some(idx) =
            prior.and_then(|key| self.rows.iter().position(|r| Self::row_key(r) == key))
        {
            self.selected = idx;
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }
}

#[path = "app_keys.rs"]
mod keys;

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
