use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use crate::store::{Liveness, StreamMeta};
use crate::tui::app::{App, Focus};
use crate::tui::rows::Row;
use crate::units;

pub fn render(frame: &mut Frame, app: &mut App) {
    let [main, status] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    let [list, content] =
        Layout::horizontal([Constraint::Length(42), Constraint::Min(1)]).areas(main);
    render_list(frame, app, list);
    render_content(frame, app, content);
    render_status(frame, app, status);
}

fn border_style(focused: bool) -> Style {
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

fn render_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let mut items: Vec<ListItem> = Vec::new();
    for row in &app.rows {
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
                        format!(" ({})", group.count),
                        Style::new().fg(Color::DarkGray),
                    ),
                ];
                if group.running {
                    spans.push(Span::raw(" "));
                    spans.push(liveness_span(Liveness::Running));
                }
                items.push(ListItem::new(Line::from(spans)));
            }
            Row::Stream(id) => {
                let Some(meta) = app.meta(id) else {
                    items.push(ListItem::new(Line::raw(format!("  {id}"))));
                    continue;
                };
                // `meta.bytes` is refreshed from the data file for live and
                // died streams by the event loop, so the size is current.
                let spans = vec![
                    Span::raw("  "),
                    liveness_span(meta.liveness()),
                    Span::raw(" "),
                    Span::raw(meta.display_name().to_string()),
                    Span::styled(
                        format!(
                            " · {} · {}",
                            crate::timefmt::clock(meta.started_at_ms),
                            units::human_bytes(meta.bytes)
                        ),
                        Style::new().fg(Color::DarkGray),
                    ),
                ];
                items.push(ListItem::new(Line::from(spans)));
            }
        }
    }
    if items.is_empty() {
        items.push(ListItem::new(Line::styled(
            "nothing captured yet",
            Style::new().fg(Color::DarkGray),
        )));
        items.push(ListItem::new(Line::styled(
            "try:  cmd | lgt myname",
            Style::new().fg(Color::DarkGray),
        )));
    }
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" streams ")
                .border_style(border_style(app.focus == Focus::List)),
        )
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_content(frame: &mut Frame, app: &mut App, area: Rect) {
    let height = usize::from(area.height.saturating_sub(2)).max(1);
    app.content_height = height;

    let title = match app
        .selected_stream_id()
        .or_else(|| app.view.as_ref().map(|v| v.id.clone()))
    {
        Some(id) => match app.meta(&id) {
            Some(meta) => content_title(app, meta),
            None => format!(" {id} "),
        },
        None => " no stream selected ".to_string(),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style(app.focus == Focus::Content));

    let Some(view) = &app.view else {
        let hint = Paragraph::new(Text::styled(
            "\n  select a stream on the left\n\n  pipe data through `lgt [name]` to capture it",
            Style::new().fg(Color::DarkGray),
        ))
        .block(block);
        frame.render_widget(hint, area);
        return;
    };

    let total = view.total_rows();
    app.scroll = app.scroll.min(total.saturating_sub(1));
    let start = app.scroll;
    let end = (start + height).min(total);
    let query = app.search_query.as_deref();
    let mut lines: Vec<Line> = Vec::new();
    for idx in start..end {
        let Some(text) = view.row(idx) else { continue };
        lines.push(content_line(text, query, app.match_line == Some(idx)));
    }
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .scroll((0, units::clamp_u16(app.hscroll)));
    frame.render_widget(paragraph, area);
}

/// Title for the content pane: name, status, live line/byte counts, duration
/// once finished, the directory the capture ran in, and the follow marker
/// (only meaningful while the stream is still running).
fn content_title(app: &App, meta: &StreamMeta) -> String {
    let liveness = meta.liveness();
    let status = match liveness {
        Liveness::Running => "running",
        Liveness::Done => "done",
        Liveness::Died => "died",
    };
    let view = app.view.as_ref().filter(|v| v.id == meta.id);
    let live_bytes = view.map(|v| v.offset).unwrap_or(meta.bytes);
    let rows = view.map(|v| v.total_rows()).unwrap_or(0);
    let duration = match (liveness, meta.ended_at_ms) {
        (Liveness::Done, Some(ended)) => format!(
            " · {}",
            units::human_duration(ended.saturating_sub(meta.started_at_ms))
        ),
        _ => String::new(),
    };
    let cwd = meta
        .cwd
        .as_deref()
        .map(|c| format!(" · {}", tilde_path(c)))
        .unwrap_or_default();
    let following = if app.follow && liveness == Liveness::Running {
        " · following"
    } else {
        ""
    };
    format!(
        " {} [{status}] {rows}L {}{duration}{cwd}{following} ",
        meta.display_name(),
        units::human_bytes(live_bytes),
    )
}

/// Shorten a path by replacing the home-directory prefix with `~`.
fn tilde_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home = home.display().to_string();
        if let Some(rest) = path.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

/// Build one content line, highlighting search matches (ASCII
/// case-insensitive so byte offsets stay valid) and emphasising the current
/// match line.
fn content_line(text: String, query: Option<&str>, is_current: bool) -> Line<'static> {
    let base = if is_current {
        Style::new().add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    };
    let Some(query) = query.filter(|q| !q.is_empty()) else {
        return Line::styled(text, base);
    };
    let lower = text.to_ascii_lowercase();
    let needle = query.to_ascii_lowercase();
    let mut spans = Vec::new();
    let mut pos = 0;
    while let Some(found) = lower[pos..].find(&needle) {
        let at = pos + found;
        if at > pos {
            spans.push(Span::styled(text[pos..at].to_string(), base));
        }
        spans.push(Span::styled(
            text[at..at + needle.len()].to_string(),
            Style::new().fg(Color::Black).bg(Color::Yellow),
        ));
        pos = at + needle.len();
        if needle.is_empty() {
            break;
        }
    }
    if pos < text.len() {
        spans.push(Span::styled(text[pos..].to_string(), base));
    }
    if spans.is_empty() {
        return Line::styled(text, base);
    }
    Line::from(spans)
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let line = match &app.search_input {
        Some(input) => Line::from(vec![
            Span::styled("/", Style::new().fg(Color::Yellow)),
            Span::raw(input.clone()),
            Span::styled("▌", Style::new().fg(Color::Yellow)),
            Span::styled(
                "  (enter to search, esc to cancel)",
                Style::new().fg(Color::DarkGray),
            ),
        ]),
        None => Line::from(vec![Span::styled(
            " q quit · tab pane · j/k move · enter open · f follow · / search · n/N match · g/G ends · ←/→ pan",
            Style::new().fg(Color::DarkGray),
        )]),
    };
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_line_highlights_matches_case_insensitively() {
        let line = content_line("Hello BETA world".to_string(), Some("beta"), false);
        let rendered: String = line.spans.iter().map(|s| s.content.clone()).collect();
        assert_eq!(rendered, "Hello BETA world");
        assert!(
            line.spans
                .iter()
                .any(|s| s.content == "BETA" && s.style.bg == Some(Color::Yellow))
        );
    }

    #[test]
    fn content_line_multibyte_is_safe() {
        let line = content_line("héllo — beta — héllo".to_string(), Some("beta"), true);
        let rendered: String = line.spans.iter().map(|s| s.content.clone()).collect();
        assert_eq!(rendered, "héllo — beta — héllo");
    }
}
