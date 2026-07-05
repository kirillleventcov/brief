use crate::span::Span;

/// Tokens are zero-copy: a `Line` token carries no text of its own. Its
/// `span` points at the trimmed line inside the `SourceMap` the lexer ran
/// over, and consumers slice the text back out via `span.range()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Line,
    Blank,
    Eof,
}

#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    pub indent: u16,
}
