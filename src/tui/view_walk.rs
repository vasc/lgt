//! Reading lines out of a [`StreamView`]: bounded per line, thanks to the
//! anchors the index keeps after every long line.

use std::fs::File;
use std::ops::Range;
use std::os::unix::fs::FileExt;

use super::{LONG, StreamView, WINDOW};

/// Where the line after the one just read starts.
enum Next {
    At(u64),
    /// The line is longer than the scan limit: look its end up in the index.
    Long,
    /// The file ends before a newline: this was the unterminated last line.
    End,
}

/// Sequential reads over `[.., limit)` through a buffer of at most
/// `capacity` bytes; nothing at or past `limit` is ever read.
pub struct Reader<'a> {
    /// Only needed to count bytes read in tests.
    #[cfg(test)]
    view: &'a StreamView,
    file: &'a File,
    limit: u64,
    buf: Vec<u8>,
    start: u64,
    len: usize,
}

impl<'a> Reader<'a> {
    pub(super) fn new(view: &'a StreamView, file: &'a File, capacity: usize, limit: u64) -> Self {
        #[cfg(not(test))]
        let _ = view;
        Reader {
            #[cfg(test)]
            view,
            file,
            limit,
            buf: vec![0u8; capacity],
            start: 0,
            len: 0,
        }
    }

    /// The bytes from `at` to the end of the buffered block (refilling it
    /// when `at` is outside), or `None` at the limit.
    pub fn window(&mut self, at: u64) -> Option<&[u8]> {
        let in_buffer = at >= self.start && at < self.start + u64::try_from(self.len).unwrap_or(0);
        if !in_buffer {
            if at >= self.limit {
                return None;
            }
            let want = usize::try_from(self.limit - at)
                .unwrap_or(usize::MAX)
                .min(self.buf.len());
            let n = loop {
                match self.file.read_at(&mut self.buf[..want], at) {
                    Ok(n) => break n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return None,
                }
            };
            if n == 0 {
                return None;
            }
            #[cfg(test)]
            self.view
                .read_bytes
                .set(self.view.read_bytes.get() + u64::try_from(n).unwrap_or(0));
            self.start = at;
            self.len = n;
        }
        let skip = usize::try_from(at - self.start).ok()?;
        Some(&self.buf[skip..self.len])
    }
}

impl StreamView {
    /// Read the line starting at `offset`, keeping at most `cap` bytes and
    /// scanning at most `max(cap, LONG)` bytes for its end.
    fn read_line(reader: &mut Reader, offset: u64, cap: usize) -> (Vec<u8>, bool, Next) {
        let scan_limit = offset + u64::try_from(cap.max(LONG)).unwrap_or(u64::MAX);
        let mut kept = Vec::new();
        let mut truncated = false;
        let mut pos = offset;
        loop {
            let Some(window) = reader.window(pos) else {
                return (kept, truncated, Next::End);
            };
            let newline = window.iter().position(|b| *b == b'\n');
            let segment = &window[..newline.unwrap_or(window.len())];
            let room = cap.saturating_sub(kept.len());
            kept.extend_from_slice(&segment[..segment.len().min(room)]);
            truncated |= segment.len() > room;
            if let Some(at) = newline {
                let next = pos + u64::try_from(at).unwrap_or(u64::MAX) + 1;
                return (kept, truncated, Next::At(next));
            }
            pos += u64::try_from(window.len()).unwrap_or(u64::MAX);
            if pos > scan_limit {
                return (kept, true, Next::Long);
            }
        }
    }

    /// Where the line after `line` starts, when `line` (starting at
    /// `offset`) was too long to scan: the index has an anchor right after
    /// every long line; otherwise scan for the newline as a last resort.
    fn after_long_line(&self, reader: &mut Reader, line: usize, offset: u64) -> Option<u64> {
        if let Some(next) = self.anchor_at(line + 1) {
            return Some(next);
        }
        let mut pos = offset;
        loop {
            let window = reader.window(pos)?;
            if let Some(at) = window.iter().position(|b| *b == b'\n') {
                return Some(pos + u64::try_from(at).ok()? + 1);
            }
            pos += u64::try_from(window.len()).ok()?;
        }
    }

    /// Call `visit(index, bytes, complete, truncated)` for each line in
    /// `range` (clamped to what is indexed), in order, until it returns
    /// false. `bytes` holds at most `max_bytes` of the line, without its
    /// newline; `truncated` says the line was longer.
    pub fn walk(
        &self,
        range: Range<usize>,
        max_bytes: usize,
        mut visit: impl FnMut(usize, &[u8], bool, bool) -> bool,
    ) {
        let end = range.end.min(self.total_rows());
        if range.start >= end {
            return;
        }
        let Some(mut reader) = self.reader(WINDOW) else {
            return;
        };
        // Every line between this anchor and `range.start` is short: a long
        // one would have put an anchor right after itself.
        let (mut idx, mut offset) = self.anchor_before(range.start);
        while idx < end {
            let visible = idx >= range.start;
            let cap = if visible { max_bytes } else { 0 };
            let (kept, truncated, next) = Self::read_line(&mut reader, offset, cap);
            let complete = idx < self.lines;
            if visible && !visit(idx, &kept, complete, truncated) {
                return;
            }
            offset = match next {
                Next::At(next) => next,
                Next::End => return,
                Next::Long if complete => match self.after_long_line(&mut reader, idx, offset) {
                    Some(next) => next,
                    None => return,
                },
                Next::Long => return,
            };
            idx += 1;
        }
    }

    /// Text of the lines in `range`, each without its line terminator and
    /// cut to `max_bytes` (enough to fill the visible width).
    pub fn rows(&self, range: Range<usize>, max_bytes: usize) -> Vec<String> {
        let mut rows = Vec::with_capacity(range.len());
        self.walk(range, max_bytes, |_, bytes, complete, truncated| {
            let text = match bytes.last() {
                Some(b'\r') if complete && !truncated => &bytes[..bytes.len() - 1],
                _ => bytes,
            };
            rows.push(String::from_utf8_lossy(text).into_owned());
            true
        });
        rows
    }

    #[cfg(test)]
    pub fn row(&self, idx: usize, max_bytes: usize) -> Option<String> {
        self.rows(idx..idx + 1, max_bytes).pop()
    }
}
