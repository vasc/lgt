use std::fs::File;
use std::ops::Range;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::PathBuf;

/// Bytes read per block while indexing.
const BLOCK: usize = 1024 * 1024;
/// Bytes read per block while rendering: enough for many short lines.
const WINDOW: usize = 16 * 1024;
/// An anchor every this many lines. Rendering walks forward from the nearest
/// anchor, so it skips at most this many (short) lines.
pub const STRIDE: usize = 64;
/// Lines longer than this get an anchor right after them, so nobody ever has
/// to read through one to find the next line.
pub const LONG: usize = 4 * 1024;

/// A view onto one stream's data file that never holds the content in
/// memory. It keeps a sparse line index — *anchors*, the byte offset of
/// every `STRIDE`-th line and of every line following a `LONG` one — built
/// incrementally in bounded chunks so a huge or growing file never stalls
/// the UI. Drawing reads only what is shown: at most `max(width, LONG)`
/// bytes of each visible line, plus at most `STRIDE` short lines before
/// them.
pub struct StreamView {
    pub id: String,
    path: PathBuf,
    file: Option<File>,
    /// `(line, offset)` pairs sorted by line; always starts with `(0, 0)`.
    anchors: Vec<(usize, u64)>,
    /// Newline-terminated lines indexed so far.
    lines: usize,
    /// Byte offset where the current, not yet terminated line starts.
    tail_start: u64,
    /// Bytes of the file scanned so far.
    indexed: u64,
    /// File length at the last check.
    known_len: u64,
    /// The file length at which reading failed; retried once it changes.
    errored_at: Option<u64>,
    /// Bytes read to render or search (tests assert reads stay bounded).
    #[cfg(test)]
    read_bytes: std::cell::Cell<u64>,
}

/// Outcome of re-checking which file the path points at.
enum FileCheck {
    Missing,
    Same,
    Reopened,
}

impl StreamView {
    pub fn new(id: String, path: PathBuf) -> Self {
        StreamView {
            id,
            path,
            file: None,
            anchors: vec![(0, 0)],
            lines: 0,
            tail_start: 0,
            indexed: 0,
            known_len: 0,
            errored_at: None,
            #[cfg(test)]
            read_bytes: std::cell::Cell::new(0),
        }
    }

    #[cfg(test)]
    pub fn take_read_bytes(&self) -> u64 {
        self.read_bytes.replace(0)
    }

    pub fn indexed_bytes(&self) -> u64 {
        self.indexed
    }

    /// True once everything currently in the file is indexed (or the file
    /// cannot be read right now, so there is nothing more to do).
    pub fn caught_up(&self) -> bool {
        self.errored_at.is_some() || self.indexed >= self.known_len
    }

    #[cfg(test)]
    pub fn anchor_count(&self) -> usize {
        self.anchors.len()
    }

    fn reset(&mut self) {
        self.anchors.clear();
        self.anchors.push((0, 0));
        self.lines = 0;
        self.tail_start = 0;
        self.indexed = 0;
        self.errored_at = None;
    }

    /// The last anchor at or before `line`.
    pub fn anchor_before(&self, line: usize) -> (usize, u64) {
        let after = self.anchors.partition_point(|(l, _)| *l <= line);
        self.anchors[after.saturating_sub(1)]
    }

    /// The anchors as (first line, last line exclusive) groups, for walking
    /// backwards one bounded group at a time.
    pub fn anchor_groups(&self) -> impl DoubleEndedIterator<Item = (usize, usize)> + '_ {
        self.anchors.iter().enumerate().map(|(k, (line, _))| {
            let next = self.anchors.get(k + 1).map_or(usize::MAX, |a| a.0);
            (*line, next)
        })
    }

    fn anchor_at(&self, line: usize) -> Option<u64> {
        self.anchors
            .binary_search_by_key(&line, |(l, _)| *l)
            .ok()
            .map(|i| self.anchors[i].1)
    }

    /// A reader over everything indexed.
    pub fn reader(&self, capacity: usize) -> Option<Reader<'_>> {
        Some(Reader::new(
            self,
            self.file.as_ref()?,
            capacity,
            self.indexed,
        ))
    }

    /// A reader over the bytes of the lines from the anchor at or before
    /// `range.start` up to `range.end`, with a buffer no bigger than that
    /// span: searching a small group never reads (or allocates) a full
    /// search window.
    pub fn reader_for(&self, range: Range<usize>, capacity: usize) -> Option<Reader<'_>> {
        let (_, start) = self.anchor_before(range.start);
        let after = self.anchors.partition_point(|(l, _)| *l < range.end);
        let limit = self
            .anchors
            .get(after)
            .map_or(self.indexed, |(_, offset)| *offset)
            .min(self.indexed);
        let span = usize::try_from(limit.saturating_sub(start)).unwrap_or(usize::MAX);
        let capacity = capacity.min(span).max(1);
        Some(Reader::new(self, self.file.as_ref()?, capacity, limit))
    }

    /// Open the file once it exists, and re-open it when the path now names
    /// a different file. A file removed from disk keeps being read through
    /// the open handle.
    fn check_file(&mut self) -> FileCheck {
        let on_disk = std::fs::metadata(&self.path).ok().map(|m| m.ino());
        let current = self
            .file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map(|m| m.ino());
        let reopen = match (on_disk, current) {
            (Some(disk), Some(ino)) => disk != ino,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if reopen && let Ok(file) = File::open(&self.path) {
            self.file = Some(file);
            self.reset();
            return FileCheck::Reopened;
        }
        if self.file.is_some() {
            FileCheck::Same
        } else {
            FileCheck::Missing
        }
    }

    /// True when the file no longer matches what was indexed although it did
    /// not shrink: truncated and rewritten past our position between checks.
    fn rewritten_underneath(&self, file: &File) -> bool {
        if self.tail_start == 0 {
            return false;
        }
        let mut byte = [0u8; 1];
        match file.read_at(&mut byte, self.tail_start - 1) {
            Ok(1) => byte[0] != b'\n',
            _ => true,
        }
    }

    /// Index up to `budget` more bytes. Returns true when the view changed.
    pub fn index_more(&mut self, budget: usize) -> bool {
        let mut changed = match self.check_file() {
            FileCheck::Missing => return false,
            FileCheck::Same => false,
            FileCheck::Reopened => true,
        };
        let Some(len) = self
            .file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map(|m| m.len())
        else {
            return changed;
        };
        self.known_len = len;
        let stale = len < self.indexed
            || self
                .file
                .as_ref()
                .is_some_and(|f| self.rewritten_underneath(f));
        if stale {
            self.reset();
            changed = true;
        }
        match self.errored_at {
            // Still failing at the same size: do not spin on it.
            Some(at) if at == len => return changed,
            // The file changed since the failure: try again.
            Some(_) => self.errored_at = None,
            None => {}
        }
        if self.indexed >= len {
            return changed;
        }
        let Some(file) = &self.file else {
            return changed;
        };
        let pending = usize::try_from(len - self.indexed).unwrap_or(usize::MAX);
        let mut buf = vec![0u8; BLOCK.min(budget.max(1)).min(pending)];
        let mut remaining = budget;
        while self.indexed < len && remaining > 0 {
            let want = buf.len().min(remaining);
            let n = match file.read_at(&mut buf[..want], self.indexed) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.errored_at = Some(len);
                    changed = true;
                    break;
                }
            };
            let base = self.indexed;
            for (offset, byte) in buf[..n].iter().enumerate() {
                if *byte == b'\n' {
                    let at = base + u64::try_from(offset).unwrap_or(u64::MAX);
                    let line_len = at - self.tail_start;
                    let next = at + 1;
                    self.lines += 1;
                    self.tail_start = next;
                    let long = line_len > u64::try_from(LONG).unwrap_or(u64::MAX);
                    if long || self.lines.is_multiple_of(STRIDE) {
                        self.anchors.push((self.lines, next));
                    }
                }
            }
            self.indexed += u64::try_from(n).unwrap_or(0);
            remaining = remaining.saturating_sub(n);
            changed = true;
        }
        changed
    }

    pub fn total_rows(&self) -> usize {
        self.lines + usize::from(self.indexed > self.tail_start)
    }
}

#[path = "view_walk.rs"]
mod walk;
pub use walk::Reader;

#[cfg(test)]
#[path = "view_tests.rs"]
pub mod tests;
