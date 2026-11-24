use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::store::{Liveness, Shelf};
use crate::tui::app::{App, Focus};
use crate::tui::content::render_content;
use crate::tui::rows::Row;
use crate::units;

/// Rows needed to show `text` wrapped at `width` columns, capped at 3.
fn banner_height(text: &str, width: u16) -> u16 {
    let needed = Line::raw(text).width().div_ceil(usize::from(width.max(1)));
    units::clamp_u16(needed.clamp(1, 3))
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let banner_text = app.advice.as_ref().map(|advice| format!(" {advice}"));
    let banner_rows = banner_text
        .as_deref()
        .map_or(0, |text| banner_height(text, frame.area().width));
    let [banner, main, status] = Layout::vertical([
        Constraint::Length(banner_rows),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let [list, content] =
        Layout::horizontal([Constraint::Length(42), Constraint::Min(1)]).areas(main);
    if let Some(text) = banner_text {
        frame.render_widget(
            Paragraph::new(Line::raw(text))
                .style(Style::new().fg(Color::Black).bg(Color::Yellow))
                .wrap(Wrap { trim: false }),
            banner,
        );
    }
    render_list(frame, app, list);
    render_content(frame, app, content);
    render_status(frame, app, status);
}

pub fn border_style(focused: bool) -> Style {
    if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    }
}

fn liveness_span(liveness: Liveness) -> Span<'static> {
    match liveness {
        Liveness::Running => Span::styled("●", Style::new().fg(Color::Green)),
        Liveness::Done => Span::styled("✔", Style::new().fg(Color::Blue)),
        Liveness::Died => Span::styled("✖", Style::new().fg(Color::Red)),
    }
}

fn row_item(app: &App, row: &Row) -> ListItem<'static> {
    match row {
        Row::Group(group) => {
            let arrow = if group.collapsed { "▸ " } else { "▾ " };
            let mut spans = vec![
                Span::styled(arrow, Style::new().fg(Color::DarkGray)),
                Span::styled(
                    group.label.clone(),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" ({})", group.members.len()),
                    Style::new().fg(Color::DarkGray),
                ),
            ];
            if group.running {
                spans.push(Span::raw(" "));
                spans.push(liveness_span(Liveness::Running));
            }
            ListItem::new(Line::from(spans))
        }
        Row::Stream(id) => {
            let Some(entry) = app.catalog.get(id) else {
                return ListItem::new(Line::raw(format!("  {id}")));
            };
            ListItem::new(Line::from(vec![
                Span::raw("  "),
                liveness_span(entry.liveness),
                Span::raw(" "),
                Span::raw(entry.meta.display_name().to_string()),
                Span::styled(
                    format!(
                        " · {} · {}",
                        crate::timefmt::clock(entry.meta.started_at_ms),
                        units::human_bytes(entry.bytes)
                    ),
                    Style::new().fg(Color::DarkGray),
                ),
            ]))
        }
    }
}

/// Scroll the list window just enough to keep the selection visible.
fn keep_selection_visible(app: &mut App, height: usize) {
    let height = height.max(1);
    if app.selected < app.list_offset {
        app.list_offset = app.selected;
    } else if app.selected >= app.list_offset + height {
        app.list_offset = app.selected + 1 - height;
    }
    let max_offset = app.rows.len().saturating_sub(height);
    app.list_offset = app.list_offset.min(max_offset);
}

/// Only the rows that fit on screen are turned into widgets, so drawing
/// costs the same for ten streams or a hundred thousand.
fn render_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let height = usize::from(area.height.saturating_sub(2));
    keep_selection_visible(app, height);
    let window_end = (app.list_offset + height).min(app.rows.len());
    let mut items: Vec<ListItem> = app.rows[app.list_offset..window_end]
        .iter()
        .map(|row| row_item(app, row))
        .collect();
    let mut state = ListState::default();
    if items.is_empty() {
        let hint = match app.shelf {
            Shelf::Active => ["nothing captured yet", "try:  cmd | lgt myname"],
            Shelf::Archive => ["the archive is empty", "try:  lgt archive 7d"],
        };
        items = hint
            .into_iter()
            .map(|text| ListItem::new(Line::styled(text, Style::new().fg(Color::DarkGray))))
            .collect();
    } else {
        state.select(Some(app.selected - app.list_offset));
    }
    let title = match app.shelf {
        Shelf::Active => format!(" streams ({}) ", app.catalog.len()),
        Shelf::Archive => format!(" archive ({}) ", app.catalog.len()),
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(border_style(app.focus == Focus::List)),
        )
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let line = if let Some(pending) = &app.pending {
        Line::styled(
            format!(" {}", pending.prompt),
            Style::new().fg(Color::Black).bg(Color::Yellow),
        )
    } else if let Some(input) = &app.search_input {
        Line::from(vec![
            Span::styled("/", Style::new().fg(Color::Yellow)),
            Span::raw(input.clone()),
            Span::styled("▌", Style::new().fg(Color::Yellow)),
            Span::styled(
                "  (enter to search, esc to cancel)",
                Style::new().fg(Color::DarkGray),
            ),
        ])
    } else if let Some(message) = &app.message {
        Line::styled(format!(" {message}"), Style::new().fg(Color::Yellow))
    } else {
        let move_key = match app.shelf {
            Shelf::Active => "a archive",
            Shelf::Archive => "a restore",
        };
        Line::styled(
            format!(
                " q quit · tab pane · j/k move · enter open · f follow · / search · n/N match · \
                 d delete · {move_key} · g/G ends · ←/→ pan"
            ),
            Style::new().fg(Color::DarkGray),
        )
    };
    frame.render_widget(Paragraph::new(line), area);
}
