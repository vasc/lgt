//! What the event loop does between frames: refresh the catalog, keep the
//! advice current, run confirmed actions, and keep the content view synced.

use std::time::Instant;

use crate::store::{Liveness, Shelf, Store};
use crate::tui::app::{Action, App};
use crate::tui::perf;
use crate::tui::view::StreamView;

/// Most bytes of the selected stream indexed between two input checks, so a
/// multi-GB stream opens progressively instead of freezing the UI.
pub const INDEX_BUDGET: usize = 8 * 1024 * 1024;

/// Load the shelf for the first time, timing everything that scales with
/// history (reading metas and building the row tree).
pub fn load(app: &mut App, store: &Store) {
    let started = Instant::now();
    app.catalog.refresh(store);
    app.sync_rows();
    app.timings
        .record_load(started.elapsed(), app.catalog.len());
    update_advice(app);
}

/// Periodic refresh; the timing covers the row-tree rebuild it may cause.
/// Returns true when something visible changed.
pub fn refresh_catalog(app: &mut App, store: &Store) -> bool {
    let started = Instant::now();
    let changed = app.catalog.refresh(store);
    if changed {
        app.sync_rows();
    }
    app.timings.refresh.record(started.elapsed());
    let advice_changed = update_advice(app);
    changed || advice_changed
}

/// Recompute the warning banner. Returns true when its text changed, so the
/// caller redraws even if nothing else did.
pub fn update_advice(app: &mut App) -> bool {
    let now = crate::timefmt::now_ms();
    let finished: Vec<i64> = app
        .catalog
        .entries()
        .filter(|e| e.liveness != Liveness::Running)
        .filter_map(|e| e.meta.finished_at_ms())
        .collect();
    let advice = perf::advice(&app.timings, app.shelf, app.catalog.len(), &finished, now);
    let changed = advice != app.advice;
    app.advice = advice;
    changed
}

/// Execute a confirmed delete/archive/restore, then refresh immediately.
pub fn perform(app: &mut App, store: &Store, action: Action) {
    let (ids, verb) = match &action {
        Action::Delete(ids) => (ids, "deleted"),
        Action::Move(ids) => (
            ids,
            match store.shelf() {
                Shelf::Active => "archived",
                Shelf::Archive => "restored",
            },
        ),
    };
    let other = store.on(match store.shelf() {
        Shelf::Active => Shelf::Archive,
        Shelf::Archive => Shelf::Active,
    });
    let mut done = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for id in ids {
        let result = match &action {
            Action::Delete(_) => store.remove_stream(id),
            Action::Move(_) => store.move_stream(id, &other),
        };
        match result {
            Ok(()) => done += 1,
            Err(err) => failures.push(format!("{err:#}")),
        }
    }
    app.message = Some(match failures.first() {
        None => format!("{verb} {done} stream(s)"),
        Some(first) => format!(
            "{verb} {done} stream(s); {} failed, first: {first}",
            failures.len()
        ),
    });
    refresh_catalog(app, store);
}

/// Keep the content view on the selected stream and pull in whatever the
/// capturing process appended. Returns true when anything visible changed.
pub fn sync_view(app: &mut App, store: &Store) -> bool {
    let mut changed = false;
    if let Some(id) = app.selected_stream_id() {
        let switch = app.view.as_ref().is_none_or(|v| v.id != id);
        if switch {
            let running = app
                .catalog
                .get(&id)
                .is_some_and(|e| e.liveness == Liveness::Running);
            let path = store.data_path(&id);
            app.view = Some(StreamView::new(id, path));
            app.scroll = 0;
            app.hscroll = 0;
            app.match_line = None;
            app.follow = running;
            changed = true;
        }
    }
    if let Some(view) = &mut app.view {
        changed |= view.index_more(INDEX_BUDGET);
        if app.follow {
            let bottom = view.total_rows().saturating_sub(app.content_height.max(1));
            if bottom != app.scroll {
                app.scroll = bottom;
                changed = true;
            }
        }
    }
    changed
}
