use super::*;

pub fn temp_file(content: &[u8]) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "lgt-view-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::write(&path, content).unwrap();
    path
}

pub fn loaded(content: &[u8]) -> StreamView {
    let mut view = StreamView::new("s".to_string(), temp_file(content));
    view.index_more(usize::MAX);
    view
}

#[test]
fn rows_split_lines_strip_crlf_and_show_partial_tail() {
    let view = loaded(b"hello\r\nworld\npart");
    assert_eq!(view.total_rows(), 3);
    assert_eq!(view.rows(0..10, 1000), vec!["hello", "world", "part"]);
    assert_eq!(view.row(3, 1000), None);
}

#[test]
fn rows_read_at_most_the_requested_bytes() {
    let view = loaded(b"abcdefghij\r\nabc\rdefgh\n");
    assert_eq!(view.row(0, 4).as_deref(), Some("abcd"));
    // Exactly fitting is not truncation: the CRLF terminator is stripped.
    assert_eq!(view.row(0, 11).as_deref(), Some("abcdefghij"));
    // A truncated line keeps its bytes as-is, even when it ends on a CR.
    assert_eq!(view.row(1, 4).as_deref(), Some("abc\r"));
}

#[test]
fn rows_start_anywhere_relative_to_checkpoints() {
    let mut content = Vec::new();
    for i in 0..500 {
        content.extend_from_slice(format!("line {i}\n").as_bytes());
    }
    let view = loaded(&content);
    assert_eq!(view.total_rows(), 500);
    for start in [0, 1, 63, 64, 65, 127, 128, 450, 499] {
        let rows = view.rows(start..start + 3, 100);
        let expected: Vec<String> = (start..(start + 3).min(500))
            .map(|i| format!("line {i}"))
            .collect();
        assert_eq!(rows, expected, "start {start}");
    }
}

#[test]
fn the_index_is_sparse() {
    let view = loaded(&b"y\n".repeat(100_000));
    assert_eq!(view.total_rows(), 100_000);
    // One checkpoint per STRIDE lines, not one offset per line.
    assert_eq!(view.anchor_count(), 100_000 / STRIDE + 1);
    assert_eq!(view.row(99_999, 10).as_deref(), Some("y"));
}

#[test]
fn indexing_is_bounded_per_call_and_resumable() {
    let content = b"aa\nbb\ncc\ndd\n";
    let mut view = StreamView::new("s".to_string(), temp_file(content));
    assert!(view.index_more(4));
    assert_eq!(view.indexed_bytes(), 4);
    assert!(!view.caught_up());
    assert_eq!(view.total_rows(), 2, "one complete line plus a partial");
    while view.index_more(4) {}
    assert!(view.caught_up());
    assert_eq!(view.total_rows(), 4);
    assert_eq!(view.row(3, 100).as_deref(), Some("dd"));
}

#[test]
fn growth_is_picked_up_and_truncation_reloads() {
    use std::io::Write;
    let path = temp_file(b"one\ntwo\n");
    let mut view = StreamView::new("s".to_string(), path.clone());
    view.index_more(usize::MAX);
    assert_eq!(view.total_rows(), 2);

    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    f.write_all(b"three\n").unwrap();
    assert!(view.index_more(usize::MAX));
    assert_eq!(view.row(2, 100).as_deref(), Some("three"));

    std::fs::write(&path, b"fresh\n").unwrap();
    assert!(view.index_more(usize::MAX));
    assert_eq!(view.total_rows(), 1);
    assert_eq!(view.row(0, 100).as_deref(), Some("fresh"));
}

#[test]
fn truncation_followed_by_regrowth_is_detected() {
    let path = temp_file(b"aaaa\nbbbb\n");
    let mut view = StreamView::new("s".to_string(), path.clone());
    view.index_more(usize::MAX);
    assert_eq!(view.total_rows(), 2);
    // Same inode, rewritten to be longer than what was indexed: the old
    // newline at offset 9 is now an `x`.
    std::fs::write(&path, b"xxxxxxxxxxxxxx\nnew\n").unwrap();
    assert!(view.index_more(usize::MAX));
    assert_eq!(view.rows(0..5, 100), vec!["xxxxxxxxxxxxxx", "new"]);
}

#[test]
fn a_replaced_file_is_reopened_even_when_empty() {
    let path = temp_file(b"old\n");
    let mut view = StreamView::new("s".to_string(), path.clone());
    view.index_more(usize::MAX);
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"").unwrap();
    assert!(
        view.index_more(usize::MAX),
        "the old content must disappear"
    );
    assert_eq!(view.total_rows(), 0);
    std::fs::write(&path, b"brand\nnew\n").unwrap();
    view.index_more(usize::MAX);
    assert_eq!(view.rows(0..2, 100), vec!["brand", "new"]);
}

#[test]
fn missing_file_is_empty_until_it_appears() {
    let path = std::env::temp_dir().join(format!("lgt-view-missing-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut view = StreamView::new("s".to_string(), path.clone());
    assert!(!view.index_more(usize::MAX));
    assert_eq!(view.total_rows(), 0);
    std::fs::write(&path, b"late\n").unwrap();
    assert!(view.index_more(usize::MAX));
    assert_eq!(view.row(0, 100).as_deref(), Some("late"));
}

#[test]
fn an_unreadable_file_stops_indexing_instead_of_spinning() {
    // A directory opens fine but every read fails (EISDIR).
    let path = temp_file(b"");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("filler"), b"x").unwrap();
    let mut view = StreamView::new("s".to_string(), path);
    view.index_more(usize::MAX);
    assert!(
        view.caught_up(),
        "a read error must not leave work pending forever"
    );
    assert!(!view.index_more(usize::MAX));
}

#[test]
fn rendering_around_huge_lines_reads_a_bounded_amount() {
    // A 4MiB single line (minified JSON, a \r progress bar) between short
    // lines: drawing must not read the whole long line, every frame.
    let mut content = b"before\n".to_vec();
    content.extend(std::iter::repeat_n(b'x', 4 * 1024 * 1024));
    content.extend_from_slice(b"\nafter\nlast\n");
    let view = loaded(&content);
    view.take_read_bytes();
    let rows = view.rows(0..4, 200);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0], "before");
    assert_eq!(rows[1], "x".repeat(200));
    assert_eq!(rows[2], "after");
    assert_eq!(rows[3], "last");
    let read = view.take_read_bytes();
    assert!(read < 256 * 1024, "read {read} bytes to draw 4 rows");
}

#[test]
fn rendering_an_unterminated_huge_line_reads_a_bounded_amount() {
    // A stream still being written as one long line, e.g. a progress bar.
    let mut content = b"start\n".to_vec();
    content.extend(std::iter::repeat_n(b'x', 4 * 1024 * 1024));
    let view = loaded(&content);
    view.take_read_bytes();
    let rows = view.rows(0..2, 100);
    assert_eq!(rows, vec!["start".to_string(), "x".repeat(100)]);
    let read = view.take_read_bytes();
    assert!(read < 256 * 1024, "read {read} bytes to draw 2 rows");
}

#[test]
fn a_failed_read_is_retried_once_the_file_changes() {
    let path = temp_file(b"");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("filler"), b"x").unwrap();
    let mut view = StreamView::new("s".to_string(), path.clone());
    view.index_more(usize::MAX);
    assert!(view.caught_up());
    assert!(!view.index_more(usize::MAX), "same size: no retry, no spin");
    for i in 0..32 {
        std::fs::write(path.join(format!("more-{i}")), b"x").unwrap();
    }
    assert!(view.index_more(usize::MAX), "a changed file is tried again");
}
