//! Shared building blocks for the C-family minifiers (Rust, Go, JS/TS,
//! Java, C/C++, SQL). Each language module produces a `Vec<Token>` with its
//! own lexer; a shared emitter consumes the stream to produce a minified
//! string with minimal-but-safe whitespace.

#![allow(dead_code)]

use super::{MinifyError, MinifyOutput, MinifyWarning};

#[derive(Debug, Clone)]
pub enum TokenKind<'a> {
    /// Identifier, keyword, or numeric literal — anything that requires a
    /// word-boundary against another adjacent word.
    Word(&'a str),
    /// Operator or punctuation. May be 1+ characters; multi-character forms
    /// like `===`, `??`, `=>`, `->`, `::` are emitted as a single Punct so
    /// the dangerous-pair table doesn't have to pull them apart again.
    Punct(&'a str),
    /// String/char literal — emitted verbatim, including delimiters.
    StrLit(&'a str),
    /// `// …` line comment body (without the leading `//` or trailing
    /// newline). The minifier emits or drops based on `keep_comments`.
    LineComment(&'a str),
    /// `/* … */` block comment body (without the surrounding delimiters).
    BlockComment(&'a str),
    /// JS template literal `` `…` `` — verbatim, including backticks.
    Template(&'a str),
    /// JS regex literal `/…/flags` — verbatim.
    Regex(&'a str),
    /// C/C++ preprocessor line, including the leading `#` and trailing line
    /// continuations. Emitted on its own line.
    Preproc(&'a str),
    /// Significant newline (used by Strategy B emitters to preserve ASI).
    Newline,
}

#[derive(Debug, Clone)]
pub struct Token<'a> {
    pub kind: TokenKind<'a>,
}

impl<'a> Token<'a> {
    pub fn new(kind: TokenKind<'a>) -> Self {
        Token { kind }
    }
}

/// True if `c` is a "word" character — the kind of glyph that, if pressed
/// against another word character with no whitespace, would form a different
/// token. ASCII alnum, `_`, `$`. Non-ASCII identifiers in Rust/Java are
/// allowed; we treat all alphabetic chars as words regardless of script.
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// True if removing whitespace between two adjacent characters would change
/// the token stream (form a multi-character operator, comment marker, or
/// merge two words). The caller passes the last char of the previously
/// emitted token and the first char of the next token.
pub fn needs_space(prev: char, next: char) -> bool {
    if is_word_char(prev) && is_word_char(next) {
        return true;
    }
    // The pairs that, if joined, become a different lexical token.
    matches!(
        (prev, next),
        ('+', '+')
            | ('-', '-')
            | ('<', '<')
            | ('>', '>')
            | ('*', '*')
            | ('/', '/')
            | ('/', '*')
            | ('*', '/')
            | (':', ':')
            | ('&', '&')
            | ('|', '|')
            | ('=', '=')
            | ('!', '=')
            | ('<', '=')
            | ('>', '=')
            | ('+', '=')
            | ('-', '=')
            | ('*', '=')
            | ('/', '=')
            | ('%', '=')
            | ('&', '=')
            | ('|', '=')
            | ('^', '=')
            | ('-', '>')
            | ('=', '>')
            | ('?', '?')
            | ('?', '.')
            | ('.', '.')
    )
}

fn last_char(s: &str) -> Option<char> {
    s.chars().next_back()
}
fn first_char(s: &str) -> Option<char> {
    s.chars().next()
}

/// Wrap a comment body in `/* … */`, defusing any `*/` inside it. A line
/// comment like `// see */ marker` would otherwise close the spliced block
/// comment early and leak the tail as live tokens.
pub fn safe_block_comment(body: &str) -> String {
    format!("/*{}*/", body.replace("*/", "* /"))
}

/// Emit a token stream stripping all whitespace and (default) all comments.
/// Used by Rust, Java, SQL.
pub fn emit_aggressive(
    tokens: &[Token<'_>],
    opts_keep_comments: bool,
) -> Result<MinifyOutput, MinifyError> {
    let mut out = String::new();
    let mut warnings: Vec<MinifyWarning> = Vec::new();
    let mut prev_emit_last: Option<char> = None;
    for tok in tokens {
        match &tok.kind {
            TokenKind::Newline => {}
            TokenKind::LineComment(body) => {
                if !opts_keep_comments {
                    continue;
                }
                let block = safe_block_comment(body);
                push_with_space(&mut out, &mut prev_emit_last, &block);
                warnings.push(MinifyWarning::LineCommentConverted);
            }
            TokenKind::BlockComment(body) => {
                if !opts_keep_comments {
                    continue;
                }
                let block = safe_block_comment(body);
                push_with_space(&mut out, &mut prev_emit_last, &block);
            }
            TokenKind::Word(s)
            | TokenKind::Punct(s)
            | TokenKind::StrLit(s)
            | TokenKind::Template(s)
            | TokenKind::Regex(s) => {
                push_with_space(&mut out, &mut prev_emit_last, s);
            }
            TokenKind::Preproc(s) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(s);
                if !s.ends_with('\n') {
                    out.push('\n');
                }
                prev_emit_last = None;
            }
        }
    }
    Ok(MinifyOutput {
        body: out,
        warnings,
    })
}

/// Emit a token stream preserving newlines (so JS/TS/Go ASI behavior is
/// preserved). Horizontal whitespace and comments are still stripped.
pub fn emit_conservative(
    tokens: &[Token<'_>],
    opts_keep_comments: bool,
) -> Result<MinifyOutput, MinifyError> {
    let mut out = String::new();
    let mut warnings: Vec<MinifyWarning> = Vec::new();
    let mut prev_emit_last: Option<char> = None;
    for tok in tokens {
        match &tok.kind {
            TokenKind::Newline => {
                // Collapse runs of newlines down to one — leading newlines
                // from prior comments shouldn't pile up.
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                prev_emit_last = None;
            }
            TokenKind::LineComment(body) => {
                if !opts_keep_comments {
                    continue;
                }
                let block = safe_block_comment(body);
                push_with_space(&mut out, &mut prev_emit_last, &block);
                warnings.push(MinifyWarning::LineCommentConverted);
            }
            TokenKind::BlockComment(body) => {
                if !opts_keep_comments {
                    continue;
                }
                let block = safe_block_comment(body);
                push_with_space(&mut out, &mut prev_emit_last, &block);
            }
            TokenKind::Word(s)
            | TokenKind::Punct(s)
            | TokenKind::StrLit(s)
            | TokenKind::Template(s)
            | TokenKind::Regex(s) => {
                push_with_space(&mut out, &mut prev_emit_last, s);
            }
            TokenKind::Preproc(s) => {
                // Preprocessor isn't really a JS/Go thing but for symmetry:
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(s);
                if !s.ends_with('\n') {
                    out.push('\n');
                }
                prev_emit_last = None;
            }
        }
    }
    Ok(MinifyOutput {
        body: out,
        warnings,
    })
}

fn push_with_space(out: &mut String, prev_emit_last: &mut Option<char>, s: &str) {
    if s.is_empty() {
        return;
    }
    if let Some(prev) = *prev_emit_last {
        if let Some(next) = first_char(s) {
            if needs_space(prev, next) {
                out.push(' ');
            }
        }
    }
    out.push_str(s);
    *prev_emit_last = last_char(s);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_word_needs_space() {
        assert!(needs_space('a', 'b'));
        assert!(needs_space('1', 'x'));
        assert!(needs_space('_', 'a'));
    }

    #[test]
    fn word_punct_no_space() {
        assert!(!needs_space('a', '('));
        assert!(!needs_space('1', ';'));
        assert!(!needs_space(')', '{'));
    }

    #[test]
    fn dangerous_pairs_need_space() {
        assert!(needs_space('+', '+'));
        assert!(needs_space('-', '-'));
        assert!(needs_space('/', '/'));
        assert!(needs_space('/', '*'));
        assert!(needs_space('=', '='));
        assert!(needs_space('!', '='));
        assert!(needs_space('<', '='));
        assert!(needs_space(':', ':'));
        assert!(needs_space('&', '&'));
        assert!(needs_space('|', '|'));
        assert!(needs_space('-', '>'));
        assert!(needs_space('=', '>'));
        assert!(needs_space('.', '.'));
    }

    #[test]
    fn safe_punct_pairs_no_space() {
        assert!(!needs_space('(', '{'));
        assert!(!needs_space(',', ' '));
        assert!(!needs_space(';', '}'));
        assert!(!needs_space(')', ';'));
        assert!(!needs_space('+', 'a'));
        assert!(!needs_space('a', ')'));
    }
}
