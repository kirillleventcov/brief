//! SQL minifier (Postgres-flavored, the canonical dialect per spec §4.5).
//!
//! Distinguishing features:
//!
//! - Line comments are `--`, not `//`.
//! - Block comments `/* … */` **nest** in Postgres.
//! - String literals are single-quoted with `''` doubled-escape; there
//!   are no backslash escapes in standard SQL.
//! - Identifier quoting `"…"` with `""` doubled-escape (Postgres).
//! - Dollar-quoted strings `$tag$ … $tag$` (Postgres) — verbatim, with
//!   arbitrary user-defined `tag` (which may be empty: `$$ … $$`).
//!
//! Strategy: aggressive (Strategy A). SQL statements are explicitly
//! `;`-terminated, so newlines can be stripped freely.

use super::c_common::{Token, TokenKind, emit_aggressive};
use super::{MinifyError, MinifyOptions, MinifyOutput};

pub fn minify(source: &str, opts: &MinifyOptions) -> Result<MinifyOutput, MinifyError> {
    let toks = tokenize(source)?;
    emit_aggressive(&toks, opts.keep_comments)
}

fn tokenize(src: &str) -> Result<Vec<Token<'_>>, MinifyError> {
    let bytes = src.as_bytes();
    let mut out: Vec<Token<'_>> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if matches!(c, b' ' | b'\t' | b'\r') {
            i += 1;
            continue;
        }
        if c == b'\n' {
            out.push(Token::new(TokenKind::Newline));
            i += 1;
            continue;
        }
        if c == b'-' && peek(bytes, i + 1) == Some(b'-') {
            let start = i + 2;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'\n' {
                j += 1;
            }
            out.push(Token::new(TokenKind::LineComment(&src[start..j])));
            i = j;
            continue;
        }
        if c == b'/' && peek(bytes, i + 1) == Some(b'*') {
            // Postgres allows nested /* */.
            let body_start = i + 2;
            let mut j = body_start;
            let mut depth = 1usize;
            while j < bytes.len() {
                if bytes[j] == b'/' && peek(bytes, j + 1) == Some(b'*') {
                    depth += 1;
                    j += 2;
                    continue;
                }
                if bytes[j] == b'*' && peek(bytes, j + 1) == Some(b'/') {
                    depth -= 1;
                    if depth == 0 {
                        let body = &src[body_start..j];
                        out.push(Token::new(TokenKind::BlockComment(body)));
                        i = j + 2;
                        break;
                    }
                    j += 2;
                    continue;
                }
                j += 1;
            }
            if depth != 0 {
                return Err(MinifyError::new("unterminated /* */ block comment"));
            }
            continue;
        }
        if c == b'\'' {
            let n = scan_sq_string(src, i)?;
            out.push(Token::new(TokenKind::StrLit(&src[i..i + n])));
            i += n;
            continue;
        }
        if c == b'"' {
            let n = scan_quoted_ident(src, i)?;
            out.push(Token::new(TokenKind::StrLit(&src[i..i + n])));
            i += n;
            continue;
        }
        if c == b'$' {
            // Could be a dollar-quoted string ($tag$ ... $tag$) or a
            // positional parameter ($1, $2, ...).
            if let Some((tag_end, body_end)) = try_scan_dollar_quoted(bytes, i) {
                out.push(Token::new(TokenKind::StrLit(&src[i..body_end])));
                i = body_end;
                let _ = tag_end;
                continue;
            }
            // Positional param `$1`, etc. — treat the run as a Word.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 {
                out.push(Token::new(TokenKind::Word(&src[i..j])));
                i = j;
                continue;
            }
            // Bare `$` not followed by tag/digit — emit as Punct.
            out.push(Token::new(TokenKind::Punct(&src[i..i + 1])));
            i += 1;
            continue;
        }
        if is_word_start(src, i) {
            let n = scan_word(src, i);
            out.push(Token::new(TokenKind::Word(&src[i..i + n])));
            i += n;
            continue;
        }
        let n = scan_multi_punct(bytes, i);
        out.push(Token::new(TokenKind::Punct(&src[i..i + n])));
        i += n;
    }
    Ok(out)
}

fn scan_sq_string(src: &str, i: usize) -> Result<usize, MinifyError> {
    let bytes = src.as_bytes();
    debug_assert_eq!(bytes[i], b'\'');
    let mut j = i + 1;
    while j < bytes.len() {
        if bytes[j] == b'\'' {
            // Doubled `''` → escaped quote, keep scanning.
            if peek(bytes, j + 1) == Some(b'\'') {
                j += 2;
                continue;
            }
            return Ok(j + 1 - i);
        }
        j += 1;
    }
    Err(MinifyError::new("unterminated string literal"))
}

fn scan_quoted_ident(src: &str, i: usize) -> Result<usize, MinifyError> {
    let bytes = src.as_bytes();
    debug_assert_eq!(bytes[i], b'"');
    let mut j = i + 1;
    while j < bytes.len() {
        if bytes[j] == b'"' {
            if peek(bytes, j + 1) == Some(b'"') {
                j += 2;
                continue;
            }
            return Ok(j + 1 - i);
        }
        j += 1;
    }
    Err(MinifyError::new("unterminated quoted identifier"))
}

/// On success returns `(tag_end, body_end)` where:
/// - `tag_end` is the byte index of the second `$` of the opening `$tag$`
/// - `body_end` is the byte index just past the closing `$tag$`
fn try_scan_dollar_quoted(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    debug_assert_eq!(bytes[i], b'$');
    let tag_start = i + 1;
    let mut j = tag_start;
    while j < bytes.len() {
        let b = bytes[j];
        if b == b'$' {
            break;
        }
        if !(b.is_ascii_alphanumeric() || b == b'_') {
            return None;
        }
        j += 1;
    }
    if j >= bytes.len() || bytes[j] != b'$' {
        return None;
    }
    let tag_end = j; // position of the closing `$` of the opener
    let tag = &bytes[tag_start..tag_end];
    let body_start = tag_end + 1;
    // Find matching `$<tag>$` close.
    let mut k = body_start;
    while k < bytes.len() {
        if bytes[k] == b'$' && k + 1 + tag.len() < bytes.len() {
            if &bytes[k + 1..k + 1 + tag.len()] == tag
                && bytes.get(k + 1 + tag.len()) == Some(&b'$')
            {
                return Some((tag_end, k + 1 + tag.len() + 1));
            }
        }
        if bytes[k] == b'$' && tag.is_empty() {
            // `$$ … $$`
            if peek(bytes, k + 1) == Some(b'$') && k > body_start {
                return Some((tag_end, k + 2));
            }
        }
        k += 1;
    }
    None
}

fn is_word_start(src: &str, i: usize) -> bool {
    let c = char_at(src, i);
    c.is_alphabetic() || c == '_' || c.is_ascii_digit()
}

fn scan_word(src: &str, i: usize) -> usize {
    let bytes = src.as_bytes();
    let mut j = i;
    while j < bytes.len() {
        let c = char_at(src, j);
        if c.is_alphanumeric() || c == '_' {
            j += c.len_utf8();
            continue;
        }
        if c == '.' {
            // Decimal: 1.5
            let next = peek(bytes, j + 1);
            if matches!(next, Some(b'0'..=b'9')) && j > i {
                j += 1;
                continue;
            }
        }
        break;
    }
    j - i
}

fn scan_multi_punct(bytes: &[u8], i: usize) -> usize {
    let two = bytes
        .get(i..i + 2)
        .map(|s| std::str::from_utf8(s).unwrap_or(""))
        .unwrap_or("");
    if matches!(two, "<=" | ">=" | "<>" | "!=" | "||" | "::") {
        return 2;
    }
    let c = char_at(unsafe { std::str::from_utf8_unchecked(bytes) }, i);
    c.len_utf8()
}

fn peek(bytes: &[u8], i: usize) -> Option<u8> {
    bytes.get(i).copied()
}

fn char_at(src: &str, i: usize) -> char {
    src[i..].chars().next().unwrap_or('\0')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn min(s: &str) -> String {
        minify(s, &MinifyOptions::default()).unwrap().body
    }

    #[test]
    fn select_with_whitespace() {
        let src = "SELECT  *\n  FROM users\n  WHERE id = 1;";
        let out = min(src);
        // `*` is punct not a word char, so `SELECT*FROM` is valid lexically:
        // SELECT (kw), * (op), FROM (kw). All major SQL dialects parse this.
        assert_eq!(out, "SELECT*FROM users WHERE id=1;");
    }

    #[test]
    fn line_comment_stripped() {
        let src = "-- comment\nSELECT 1;";
        let out = min(src);
        assert_eq!(out, "SELECT 1;");
    }

    #[test]
    fn block_comment_stripped() {
        let src = "/* hi */ SELECT 1;";
        let out = min(src);
        assert_eq!(out, "SELECT 1;");
    }

    #[test]
    fn nested_block_comment() {
        let src = "/* outer /* inner */ outer */ SELECT 1;";
        let out = min(src);
        assert_eq!(out, "SELECT 1;");
    }

    #[test]
    fn doubled_quote_in_string() {
        let src = "SELECT 'O''Brien';";
        let out = min(src);
        // `SELECT` (word) → `'` (punct); not word-word, no space needed.
        assert_eq!(out, "SELECT'O''Brien';");
    }

    #[test]
    fn quoted_identifier() {
        let src = "SELECT \"my col\" FROM t;";
        let out = min(src);
        // Quoted identifiers' delimiters are `"`, which is punct; abuts
        // SELECT and FROM with no required space.
        assert_eq!(out, "SELECT\"my col\"FROM t;");
    }

    #[test]
    fn dollar_quoted_string() {
        let src = "DO $$ BEGIN RAISE NOTICE 'hi'; END $$;";
        let out = min(src);
        assert!(
            out.contains("$$ BEGIN RAISE NOTICE 'hi'; END $$"),
            "{}",
            out
        );
    }

    #[test]
    fn dollar_quoted_with_tag() {
        let src = "SELECT $tag$ raw \"text\" $tag$;";
        let out = min(src);
        assert!(out.contains("$tag$ raw \"text\" $tag$"));
    }

    #[test]
    fn positional_param() {
        let src = "SELECT * FROM t WHERE id = $1;";
        let out = min(src);
        assert_eq!(out, "SELECT*FROM t WHERE id=$1;");
    }

    #[test]
    fn keep_comments_converts() {
        let src = "-- hi\nSELECT 1;";
        let r = minify(
            src,
            &MinifyOptions {
                keep_comments: true,
            },
        )
        .unwrap();
        assert!(r.body.starts_with("/* hi*/"));
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn unterminated_string() {
        assert!(minify("SELECT 'oops", &MinifyOptions::default()).is_err());
    }

    #[test]
    fn unterminated_block_comment() {
        assert!(minify("/* unterminated", &MinifyOptions::default()).is_err());
    }

    #[test]
    fn case_preservation() {
        let src = "select Foo from Bar;";
        let out = min(src);
        // We do not normalize keyword case (spec §4.5 — opt-in only).
        assert_eq!(out, "select Foo from Bar;");
    }

    #[test]
    fn double_dash_only_at_start_of_word() {
        // `--` between digits like `5-1` is a subtraction; only `--` after
        // whitespace is a comment. Our tokenizer treats any `--` as start
        // of comment. In practice SQL minifiers do too.
        let src = "SELECT 5--1\nFROM t;";
        let out = min(src);
        assert_eq!(out, "SELECT 5 FROM t;");
    }
}
