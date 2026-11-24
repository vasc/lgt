mod actions;
mod app;
mod catalog;
mod content;
mod perf;
mod rows;
mod ui;
mod view;
mod view_scan;

use std::io::IsTerminal;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use ratatui::crossterm::event::{self, Event};

use crate::store::Store;
use app::App;

/// How often the stream list is re-scanned for new, finished or removed
/// streams.
const CATALOG_EVERY: Duration = Duration::from_millis(1000);
/// How long to wait for input before checking the selected stream for new
/// data again.
const TICK: Duration = Duration::from_millis(200);

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
    let mut app = App::new(store.shelf());
    actions::load(&mut app, store);
    let mut last_catalog = Instant::now();
    let mut dirty = true;

    loop {
        if last_catalog.elapsed() >= CATALOG_EVERY {
            // Running streams' sizes move even when the tree does not.
            dirty |= actions::refresh_catalog(&mut app, store) || app.catalog.any_running();
            last_catalog = Instant::now();
        }
        dirty |= actions::sync_view(&mut app, store);
        if dirty {
            let started = Instant::now();
            terminal.draw(|frame| ui::render(frame, &mut app))?;
            app.timings.frame.record(started.elapsed());
            dirty = false;
        }

        // Keep indexing a large stream without waiting, but still let input
        // in between chunks.
        let busy = app.view.as_ref().is_some_and(|v| !v.caught_up());
        let timeout = if busy { Duration::ZERO } else { TICK };
        if !event::poll(timeout)? {
            continue;
        }
        let ready = || -> Result<Option<Event>> {
            Ok(if event::poll(Duration::ZERO)? {
                Some(event::read()?)
            } else {
                None
            })
        };
        match handle_events(&mut app, store, ready)? {
            Batch::Quit => return Ok(()),
            Batch::Handled { acted } => {
                dirty = true;
                if acted {
                    last_catalog = Instant::now();
                }
            }
        }
    }
}

enum Batch {
    Quit,
    /// `acted`: a confirmed action ran (and refreshed the catalog).
    Handled {
        acted: bool,
    },
}

/// Handle every event that is already queued before the next frame:
/// holding a key down must not cost one full redraw per repeat.
fn handle_events(
    app: &mut App,
    store: &Store,
    mut ready: impl FnMut() -> Result<Option<Event>>,
) -> Result<Batch> {
    let mut acted = false;
    while let Some(event) = ready()? {
        if let Event::Key(key) = event
            && let Some(action) = app.on_key(key)
        {
            actions::perform(app, store, action);
            acted = true;
        }
        if app.quit {
            return Ok(Batch::Quit);
        }
    }
    Ok(Batch::Handled { acted })
}

#[cfg(test)]
mod tests;
