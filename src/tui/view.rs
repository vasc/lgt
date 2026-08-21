/// Incrementally loaded view of one stream's data file. Bytes are split into
/// lines as they arrive; a trailing unterminated line is kept in `partial`
/// and shown as the last row.
pub struct StreamView {
    pub id: String,
    pub offset: u64,
    pub partial: Vec<u8>,
    pub lines: Vec<String>,
}

impl StreamView {
    pub fn new(id: String) -> Self {
        StreamView {
            id,
            offset: 0,
            partial: Vec::new(),
            lines: Vec::new(),
        }
    }

    pub fn total_rows(&self) -> usize {
        self.lines.len() + usize::from(!self.partial.is_empty())
    }

    pub fn row(&self, idx: usize) -> Option<String> {
        if idx < self.lines.len() {
            return Some(self.lines[idx].clone());
        }
        if idx == self.lines.len() && !self.partial.is_empty() {
            return Some(String::from_utf8_lossy(&self.partial).into_owned());
        }
        None
    }

    pub fn append(&mut self, chunk: &[u8]) {
        let mut data = std::mem::take(&mut self.partial);
        data.extend_from_slice(chunk);
        let mut start = 0;
        for (i, b) in data.iter().enumerate() {
            if *b == b'\n' {
                let mut end = i;
                if end > start && data[end - 1] == b'\r' {
                    end -= 1;
                }
                self.lines
                    .push(String::from_utf8_lossy(&data[start..end]).into_owned());
                start = i + 1;
            }
        }
        self.partial = data[start..].to_vec();
    }

    pub fn reset(&mut self) {
        self.offset = 0;
        self.partial.clear();
        self.lines.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_appends_split_lines_and_partial() {
        let mut view = StreamView::new("s".to_string());
        view.append(b"hello\nwor");
        assert_eq!(view.lines, vec!["hello"]);
        assert_eq!(view.total_rows(), 2);
        assert_eq!(view.row(1).as_deref(), Some("wor"));
        view.append(b"ld\r\n");
        assert_eq!(view.lines, vec!["hello", "world"]);
        assert_eq!(view.total_rows(), 2);
    }
}
