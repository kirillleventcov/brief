//! Line tokens produced by [`crate::lexer`] and consumed by
//! [`crate::parser`].

use crate::span::Span;

/// What a [`Token`] represents.
///
/// Tokens are zero-copy: a `Line` token carries no text of its own. Its
/// `span` points at the trimmed line inside the `SourceMap` the lexer ran
/// over, and consumers slice the text back out via `span.range()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TokenKind {
    /// A line with visible content.
    Line,
    /// A line that is empty or holds only whitespace.
    Blank,
    /// End of input; always the last token.
    Eof,
}

/// One source line, as classified by the lexer.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Token {
    pub kind: TokenKind,
    /// The line with trailing spaces and `\r` trimmed; empty for `Eof`.
    pub span: Span,
    /// Number of leading spaces; 0 for `Blank` and `Eof`.
    pub indent: u16,
}
