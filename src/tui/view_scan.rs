//! Searching a [`StreamView`] without loading it: the file is streamed
//! through a fixed-size window, so even a multi-GB single line is searched
//! in constant memory.

use std::ops::Range;

use crate::tui::view::StreamView;

/// Bytes read per block while searching.
const SEARCH_WINDOW: usize = 1024 * 1024;

fn contains_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len()
        && haystack
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle))
}

impl StreamView {
    /// First line in `range` (the last one when `reverse`) whose text
    /// contains `needle`, compared ASCII-case-insensitively. Only indexed
    /// lines are searched; callers finish indexing first when they need the
    /// whole stream.
    pub fn find(&self, range: Range<usize>, reverse: bool, needle: &str) -> Option<usize> {
        let needle = needle.as_bytes();
        if needle.is_empty() || needle.contains(&b'\n') {
            return None;
        }
        if !reverse {
            return self.scan(range, needle, true);
        }
        // Backwards: one anchor group at a time, newest first, keeping the
        // last match inside each. Groups are bounded, so this never streams
        // the whole file to find a match near the end.
        let end = range.end.min(self.total_rows());
        for (group_start, group_end) in self.anchor_groups().rev() {
            if group_start >= end {
                continue;
            }
            let sub = group_start.max(range.start)..group_end.min(end);
            if sub.start < sub.end
                && let Some(found) = self.scan(sub, needle, false)
            {
                return Some(found);
            }
            if group_start <= range.start {
                break;
            }
        }
        None
    }

    /// Stream the lines in `range` and return the first (or, with
    /// `first_only` unset, the last) one containing `needle`. A match can
    /// span read windows: the last `needle.len() - 1` bytes of the current
    /// line are carried over.
    fn scan(&self, range: Range<usize>, needle: &[u8], first_only: bool) -> Option<usize> {
        let end = range.end.min(self.total_rows());
        if range.start >= end {
            return None;
        }
        let mut reader = self.reader_for(range.start..end, SEARCH_WINDOW)?;
        let (mut idx, mut pos) = self.anchor_before(range.start);
        let keep = needle.len() - 1;
        let mut carry: Vec<u8> = Vec::with_capacity(keep * 2);
        let mut line_matched = false;
        let mut found = None;
        while idx < end {
            let Some(window) = reader.window(pos) else {
                break;
            };
            let read = window.len();
            let mut i = 0;
            while i < read {
                let rest = &window[i..];
                let newline = rest.iter().position(|b| *b == b'\n');
                let segment = &rest[..newline.unwrap_or(rest.len())];
                if idx >= range.start && !line_matched {
                    // Matches across the carried tail, then within the segment.
                    let head = &segment[..segment.len().min(keep)];
                    let mut joined = carry.clone();
                    joined.extend_from_slice(head);
                    if contains_ignore_ascii_case(&joined, needle)
                        || contains_ignore_ascii_case(segment, needle)
                    {
                        line_matched = true;
                        found = Some(idx);
                        if first_only {
                            return found;
                        }
                    }
                    if segment.len() >= keep {
                        carry.clear();
                        carry.extend_from_slice(&segment[segment.len() - keep..]);
                    } else {
                        carry.extend_from_slice(segment);
                        let excess = carry.len().saturating_sub(keep);
                        carry.drain(..excess);
                    }
                }
                match newline {
                    Some(at) => {
                        idx += 1;
                        carry.clear();
                        line_matched = false;
                        i += at + 1;
                        if idx >= end {
                            return found;
                        }
                    }
                    None => i = read,
                }
            }
            pos += u64::try_from(read).ok()?;
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use crate::tui::view::tests::loaded;

    #[test]
    fn find_matches_both_directions_case_insensitively() {
        let view = loaded(b"alpha\nBeta\ngamma\nbeta again\n");
        assert_eq!(view.find(0..4, false, "beta"), Some(1));
        assert_eq!(view.find(2..4, false, "beta"), Some(3));
        assert_eq!(view.find(0..4, true, "BETA"), Some(3));
        assert_eq!(view.find(0..3, true, "beta"), Some(1));
        assert_eq!(view.find(0..4, false, "delta"), None);
        assert_eq!(view.find(0..4, false, ""), None);
    }

    #[test]
    fn find_covers_the_unterminated_last_line() {
        let view = loaded(b"one\ntwo\nthe end");
        assert_eq!(view.find(0..3, false, "end"), Some(2));
        assert_eq!(view.find(0..3, true, "end"), Some(2));
    }

    #[test]
    fn find_crosses_anchors_and_blocks() {
        let mut content = Vec::new();
        for i in 0..40_000 {
            content.extend_from_slice(
                format!("filler line number {i:08} ......................\n").as_bytes(),
            );
        }
        content.extend_from_slice(b"the NEEDLE is here\n");
        let view = loaded(&content);
        let all = 0..view.total_rows();
        assert_eq!(view.find(all.clone(), false, "needle"), Some(40_000));
        assert_eq!(view.find(all.clone(), true, "number 00000001 "), Some(1));
        assert_eq!(view.find(all.clone(), true, "needle"), Some(40_000));
        // A range starting mid-group must not match lines before it.
        assert_eq!(view.find(130..200, false, "number 00000129 "), None);
        assert_eq!(view.find(130..200, true, "number 00000129 "), None);
        assert_eq!(view.find(130..200, true, "number 00000130 "), Some(130));
    }

    #[test]
    fn a_match_split_across_read_windows_is_found() {
        // Place the needle so it straddles the 1MiB search window boundary.
        let window = super::SEARCH_WINDOW;
        let mut content = vec![b'x'; window - 3];
        content.extend_from_slice(b"NEEDLE");
        content.extend_from_slice(b"\nafter\n");
        let view = loaded(&content);
        assert_eq!(view.find(0..2, false, "needle"), Some(0));
        assert_eq!(view.find(0..2, true, "needle"), Some(0));
    }

    #[test]
    fn searching_a_huge_single_line_uses_constant_memory_and_finds_the_end() {
        let mut content = b"head\n".to_vec();
        content.extend(std::iter::repeat_n(b'x', 8 * 1024 * 1024));
        content.extend_from_slice(b"tail-marker\nnext\n");
        let view = loaded(&content);
        assert_eq!(view.find(0..3, false, "tail-marker"), Some(1));
        assert_eq!(view.find(0..3, true, "tail-marker"), Some(1));
        assert_eq!(view.find(0..3, false, "next"), Some(2));
    }

    #[test]
    fn needles_with_newlines_match_nothing() {
        let view = loaded(b"a\nb\n");
        assert_eq!(view.find(0..2, false, "a\nb"), None);
    }
}

#[cfg(test)]
mod read_bounds {
    use crate::tui::view::tests::loaded;

    #[test]
    fn reverse_search_reads_each_byte_about_once() {
        // 20k short lines and no match: backwards search walks every anchor
        // group; it must read roughly the file once, not a full search
        // window per group.
        let mut content = Vec::new();
        for i in 0..20_000 {
            content.extend_from_slice(format!("line {i:06} of filler text\n").as_bytes());
        }
        let size = u64::try_from(content.len()).unwrap();
        let view = loaded(&content);
        view.take_read_bytes();
        assert_eq!(view.find(0..view.total_rows(), true, "zzz-not-there"), None);
        let read = view.take_read_bytes();
        assert!(
            read <= size * 2,
            "reverse search read {read} bytes of a {size}-byte stream"
        );
    }
}
