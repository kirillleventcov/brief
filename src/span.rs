use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub len: u32,
}

impl Span {
    pub const DUMMY: Span = Span { start: 0, len: 0 };

    pub fn new(start: usize, len: usize) -> Self {
        Span {
            start: start as u32,
            len: len as u32,
        }
    }

    pub fn end(&self) -> usize {
        (self.start + self.len) as usize
    }
    pub fn range(&self) -> Range<usize> {
        self.start as usize..self.end()
    }

    pub fn join(self, other: Span) -> Span {
        let start = self.start.min(other.start);
        let end = (self.start + self.len).max(other.start + other.len);
        Span {
            start,
            len: end - start,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SourceMap {
    pub path: String,
    pub source: String,
    line_starts: Vec<u32>,
}

impl SourceMap {
    pub fn new(path: impl Into<String>, source: impl Into<String>) -> Self {
        let source = source.into();
        let mut line_starts = vec![0u32];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push((i + 1) as u32);
            }
        }
        SourceMap {
            path: path.into(),
            source,
            line_starts,
        }
    }

    pub fn line_col(&self, offset: u32) -> (usize, usize) {
        let idx = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let line_start = self.line_starts[idx];
        let prefix = &self.source.as_bytes()[line_start as usize..offset as usize];
        let col = std::str::from_utf8(prefix)
            .map(|s| s.chars().count())
            .unwrap_or(0)
            + 1;
        (idx + 1, col)
    }

    pub fn line_text(&self, line: usize) -> &str {
        let start = self.line_starts[line - 1] as usize;
        let end = self
            .line_starts
            .get(line)
            .map(|n| *n as usize)
            .unwrap_or(self.source.len());
        let s = &self.source[start..end];
        s.trim_end_matches('\n').trim_end_matches('\r')
    }
}
