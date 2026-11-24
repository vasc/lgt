use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::store::Liveness;
use crate::tui::app::{App, Focus};
use crate::tui::catalog::Entry;
use crate::tui::ui::border_style;
use crate::units;

pub fn render_content(frame: &mut Frame, app: &mut App, area: Rect) {
    let height = usize::from(area.height.saturating_sub(2)).max(1);
    let width = usize::from(area.width.saturating_sub(2));
    app.content_height = height;

    let title = match app
        .selected_stream_id()
        .or_else(|| app.view.as_ref().map(|v| v.id.clone()))
    {
        Some(id) => match app.catalog.get(&id) {
            Some(entry) => content_title(app, entry),
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
    if app.follow {
        // Only the renderer knows the pane height for certain (the first
        // frame after opening a stream has not measured it yet).
        app.scroll = total.saturating_sub(height);
    }
    app.scroll = app.scroll.min(total.saturating_sub(1));
    let start = app.scroll;
    let end = (start + height).min(total);
    // Read just enough of each line to fill the pane at the current pan
    // offset (4 bytes per column covers any UTF-8 character).
    let max_bytes = app
        .hscroll
        .saturating_add(width)
        .saturating_mul(4)
        .saturating_add(16);
    let query = app.search_query.as_deref();
    let lines: Vec<Line> = view
        .rows(start..end, max_bytes)
        .into_iter()
        .zip(start..)
        .map(|(text, idx)| content_line(text, query, app.match_line == Some(idx)))
        .collect();
    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .scroll((0, units::clamp_u16(app.hscroll)));
    frame.render_widget(paragraph, area);
}

/// Title for the content pane: name, status, line/byte counts, duration once
/// finished, the directory the capture ran in, and the follow marker (only
/// meaningful while the stream is still running).
fn content_title(app: &App, entry: &Entry) -> String {
    let meta = &entry.meta;
    let status = match entry.liveness {
        Liveness::Running => "running",
        Liveness::Done => "done",
        Liveness::Died => "died",
    };
    let view = app.view.as_ref().filter(|v| v.id == meta.id);
    let indexing = view.is_some_and(|v| !v.caught_up());
    let rows = match view {
        Some(v) if indexing => format!("{}L…", v.total_rows()),
        Some(v) => format!("{}L", v.total_rows()),
        None => "0L".to_string(),
    };
    let bytes = match view {
        Some(v) if v.caught_up() => v.indexed_bytes(),
        _ => entry.bytes,
    };
    let duration = match (entry.liveness, meta.ended_at_ms) {
        (Liveness::Running, _) | (_, None) => String::new(),
        (_, Some(ended)) => format!(
            " · {}",
            units::human_duration(ended.saturating_sub(meta.started_at_ms))
        ),
    };
    let cwd = meta
        .cwd
        .as_deref()
        .map(|c| format!(" · {}", tilde_path(c)))
        .unwrap_or_default();
    let following = if app.follow && entry.liveness == Liveness::Running {
        " · following"
    } else {
        ""
    };
    format!(
        " {} [{status}] {rows} {}{duration}{cwd}{following} ",
        meta.display_name(),
        units::human_bytes(bytes),
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
    }
    if pos < text.len() {
        spans.push(Span::styled(text[pos..].to_string(), base));
    }
    if spans.is_empty() {
        return Line::styled(text, base);
    }
    Line::from(spans)
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
