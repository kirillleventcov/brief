//! Byte spans into Brief source and the [`SourceMap`] that resolves them to
//! line/column positions for diagnostics.

use std::ops::Range;
use std::str;

/// A byte range `start..start + len` into a document's source text.
///
/// Every AST node and diagnostic carries one so tools can point back at the
/// exact source that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub start: u32,
    pub len: u32,
}

impl Span {
    /// Placeholder span (`0..0`) for nodes that have no source location,
    /// such as synthesized test fixtures.
    pub const DUMMY: Span = Span { start: 0, len: 0 };

    /// Creates a span covering `len` bytes from byte offset `start`.
    #[must_use]
    pub fn new(start: usize, len: usize) -> Self {
        Span {
            start: start as u32,
            len: len as u32,
        }
    }

    /// Byte offset one past the last byte of the span.
    #[must_use]
    pub fn end(&self) -> usize {
        (self.start + self.len) as usize
    }

    /// The span as a byte range, ready for slicing the source text.
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        self.start as usize..self.end()
    }

    /// Smallest span covering both `self` and `other`, including any gap
    /// between them.
    #[must_use]
    pub fn join(self, other: Span) -> Span {
        let start = self.start.min(other.start);
        let end = (self.start + self.len).max(other.start + other.len);
        Span {
            start,
            len: end - start,
        }
    }
}

/// A 1-based line and column position, as shown to users in diagnostics.
///
/// `column` counts characters (Unicode scalar values), not bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineColumn {
    pub line: usize,
    pub column: usize,
}

/// A document's path and source text, indexed by line so byte offsets can
/// be mapped to [`LineColumn`] positions.
#[derive(Debug, Clone)]
pub struct SourceMap {
    /// Path used when reporting diagnostics; not read from disk.
    pub path: String,
    /// Full source text that spans index into.
    pub source: String,
    line_starts: Vec<u32>,
}

impl SourceMap {
    /// Indexes `source`, labelling it with `path` for diagnostics.
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

    /// 1-based line and column of byte `offset`.
    ///
    /// # Panics
    ///
    /// Panics if `offset` is past the end of the source.
    #[must_use]
    pub fn line_column(&self, offset: u32) -> LineColumn {
        let idx = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let line_start = self.line_starts[idx];
        let prefix = &self.source.as_bytes()[line_start as usize..offset as usize];
        let column = str::from_utf8(prefix)
            .map(|s| s.chars().count())
            .unwrap_or(0)
            + 1;
        LineColumn {
            line: idx + 1,
            column,
        }
    }

    /// Text of 1-based line `line`, without its `\n` or `\r\n` terminator.
    ///
    /// # Panics
    ///
    /// Panics if `line` is 0 or greater than the number of lines.
    #[must_use]
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
