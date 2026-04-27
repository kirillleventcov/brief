use crate::diag::{Code, Diagnostic};
use crate::span::{SourceMap, Span};
use crate::token::{Token, TokenKind};

pub fn lex(src: &SourceMap) -> Result<Vec<Token>, Vec<Diagnostic>> {
    let mut diags = Vec::new();
    let bytes = src.source.as_bytes();

    for (i, w) in bytes.windows(3).enumerate() {
        if i > 0 && w == [0xEF, 0xBB, 0xBF] {
            diags.push(Diagnostic::new(Code::BomNotAtStart, Span::new(i, 3)));
        }
    }

    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\t' {
            diags.push(
                Diagnostic::new(Code::TabCharacter, Span::new(i, 1))
                    .help("tabs are forbidden; use two spaces per indent level"),
            );
        }
    }

    let source = &src.source;
    let mut tokens = Vec::new();
    let mut line_start = 0usize;
    let mut i = 0usize;
    let bytes = source.as_bytes();
    while i <= bytes.len() {
        if i == bytes.len() || bytes[i] == b'\n' {
            if i == bytes.len() && line_start == i {
                break;
            }
            let raw = &source[line_start..i];
            let trimmed = raw.trim_end_matches(|c: char| c == ' ' || c == '\r');
            let indent = leading_spaces(trimmed);
            let span = Span::new(line_start, trimmed.len());
            if trimmed.trim().is_empty() {
                tokens.push(Token {
                    kind: TokenKind::Blank,
                    span,
                    indent: 0,
                });
            } else {
                tokens.push(Token {
                    kind: TokenKind::Line(trimmed.to_string()),
                    span,
                    indent: indent as u16,
                });
            }
            line_start = i + 1;
            if i == bytes.len() {
                break;
            }
        }
        i += 1;
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span::new(bytes.len(), 0),
        indent: 0,
    });

    if diags.is_empty() {
        Ok(tokens)
    } else {
        Err(diags)
    }
}

fn leading_spaces(s: &str) -> usize {
    s.bytes().take_while(|b| *b == b' ').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexes_simple_lines() {
        let src = SourceMap::new("doc.brf", "# Hello\n\nWorld\n");
        let toks = lex(&src).unwrap();
        assert!(matches!(toks[0].kind, TokenKind::Line(ref s) if s == "# Hello"));
        assert!(matches!(toks[1].kind, TokenKind::Blank));
        assert!(matches!(toks[2].kind, TokenKind::Line(ref s) if s == "World"));
        assert!(matches!(toks[3].kind, TokenKind::Eof));
    }

    #[test]
    fn rejects_tabs() {
        let src = SourceMap::new("doc.brf", "  \tx\n");
        let err = lex(&src).unwrap_err();
        assert_eq!(err[0].code, Code::TabCharacter);
    }

    #[test]
    fn strips_trailing_whitespace() {
        let src = SourceMap::new("doc.brf", "hi   \n");
        let toks = lex(&src).unwrap();
        assert!(matches!(toks[0].kind, TokenKind::Line(ref s) if s == "hi"));
    }

    #[test]
    fn normalizes_crlf() {
        let src = SourceMap::new("doc.brf", "a\r\nb\r\n");
        let toks = lex(&src).unwrap();
        if let TokenKind::Line(s) = &toks[0].kind {
            assert_eq!(s, "a");
        } else {
            panic!();
        }
        if let TokenKind::Line(s) = &toks[1].kind {
            assert_eq!(s, "b");
        } else {
            panic!();
        }
    }

    #[test]
    fn computes_indent() {
        let src = SourceMap::new("doc.brf", "  - a\n");
        let toks = lex(&src).unwrap();
        assert_eq!(toks[0].indent, 2);
    }
}
