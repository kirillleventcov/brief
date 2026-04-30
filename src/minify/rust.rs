//! Rust minifier.
//!
//! Handles the lexical exotica that distinguishes Rust from "generic
//! C-family":
//!
//! - Raw strings `r"…"`, `r#"…"#`, `r##"…"##` (any `#` count)
//! - Byte strings `b"…"`, raw byte strings `br"…"`, `br#"…"#`
//! - Char literals `'a'`, `'\n'`, `'\u{1234}'`, `b'a'`
//! - Lifetimes `'static`, `'a`, `'_` — distinguished from char literals
//!   by the absence of a closing apostrophe after a single ident token
//! - Block comments that **nest**: `/* outer /* inner */ outer */`
//! - Underscored numeric literals (`1_000_000`), suffixed numbers
//!   (`1u32`), hex/oct/bin (`0xFF`, `0b10`)
//! - Doc comments `///` and `//!` are line comments to us — stripped by
//!   default per spec §4.2 (which warns this is intentional). Use
//!   `@minify-keep-comments` to retain.
//!
//! Strategy: aggressive (Strategy A) — strip newlines and all
//! non-required whitespace.

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
        // Whitespace and newlines.
        if matches!(c, b' ' | b'\t' | b'\r') {
            i += 1;
            continue;
        }
        if c == b'\n' {
            out.push(Token::new(TokenKind::Newline));
            i += 1;
            continue;
        }
        // Comments.
        if c == b'/' && peek(bytes, i + 1) == Some(b'/') {
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
            // Nested block comment.
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
        // Raw / byte / byte-raw strings.
        if c == b'r' || c == b'b' {
            if let Some((tok, n)) = try_scan_special_string(src, i)? {
                out.push(Token::new(TokenKind::StrLit(tok)));
                i += n;
                continue;
            }
        }
        // Regular string.
        if c == b'"' {
            let n = scan_dq_string(src, i)?;
            out.push(Token::new(TokenKind::StrLit(&src[i..i + n])));
            i += n;
            continue;
        }
        // Char literal vs lifetime.
        if c == b'\'' {
            let (kind, n) = scan_quote(src, i)?;
            match kind {
                QuoteKind::Char => out.push(Token::new(TokenKind::StrLit(&src[i..i + n]))),
                QuoteKind::Lifetime => out.push(Token::new(TokenKind::Word(&src[i..i + n]))),
            }
            i += n;
            continue;
        }
        // Word: identifier or number. Identifiers can include non-ASCII
        // (Rust permits XID identifiers); we accept all alphanumerics here
        // because we're not validating, just lexing.
        if is_word_start(src, i) {
            let n = scan_word(src, i);
            out.push(Token::new(TokenKind::Word(&src[i..i + n])));
            i += n;
            continue;
        }
        // Punctuation. Multi-char operators must be lexed as one token,
        // because the emitter inserts a space between any two punct chars
        // that would form a different operator if joined (the dangerous-
        // pair table). Without this, `->` written as two single-char Puncts
        // would render `- >` because `-`/`>` is in the table.
        let n = scan_multi_punct(bytes, i);
        out.push(Token::new(TokenKind::Punct(&src[i..i + n])));
        i += n;
    }
    Ok(out)
}

fn scan_multi_punct(bytes: &[u8], i: usize) -> usize {
    let three = bytes
        .get(i..i + 3)
        .map(|s| std::str::from_utf8(s).unwrap_or(""))
        .unwrap_or("");
    let two = bytes
        .get(i..i + 2)
        .map(|s| std::str::from_utf8(s).unwrap_or(""))
        .unwrap_or("");
    if matches!(three, "..=" | "<<=" | ">>=") {
        return 3;
    }
    if matches!(
        two,
        "->" | "=>"
            | "::"
            | "=="
            | "!="
            | "<="
            | ">="
            | "&&"
            | "||"
            | "<<"
            | ">>"
            | ".."
            | "+="
            | "-="
            | "*="
            | "/="
            | "%="
            | "&="
            | "|="
            | "^="
    ) {
        return 2;
    }
    // Single byte (UTF-8 punctuation isn't expected in Rust source).
    let c = char_at(unsafe { std::str::from_utf8_unchecked(bytes) }, i);
    c.len_utf8()
}

#[derive(Debug)]
enum QuoteKind {
    Char,
    Lifetime,
}

fn try_scan_special_string(src: &str, i: usize) -> Result<Option<(&str, usize)>, MinifyError> {
    let bytes = src.as_bytes();
    let mut p = i;
    let mut byte = false;
    if bytes[p] == b'b' {
        // `b"…"` / `br"…"` / `br#"…"#` / `b'…'`
        if peek(bytes, p + 1) == Some(b'\'') {
            // Byte char literal — handled by scan_quote later.
            return Ok(None);
        }
        byte = true;
        p += 1;
    }
    let mut raw = false;
    if peek(bytes, p) == Some(b'r') && p > i {
        // `br…` so far
        raw = true;
        p += 1;
    } else if !byte && peek(bytes, p) == Some(b'r') {
        raw = true;
        p += 1;
    }
    // Count `#`s if raw.
    let mut hashes = 0usize;
    if raw {
        while peek(bytes, p) == Some(b'#') {
            hashes += 1;
            p += 1;
        }
    }
    // Must now see `"`. If not, this was not a special string — back out.
    if peek(bytes, p) != Some(b'"') {
        // For `b` followed by ident chars (not `"`/`'`), it's just a normal
        // identifier. Same for `r` not followed by `"` or `#"`.
        return Ok(None);
    }
    // We are committed: scan the body.
    let body_start = p + 1;
    if raw {
        // Find the next `"` followed by `hashes` `#`s.
        let mut j = body_start;
        loop {
            if j >= bytes.len() {
                return Err(MinifyError::new("unterminated raw string literal"));
            }
            if bytes[j] == b'"' {
                // Check for matching # count.
                let mut k = j + 1;
                let mut found = 0;
                while k < bytes.len() && bytes[k] == b'#' && found < hashes {
                    found += 1;
                    k += 1;
                }
                if found == hashes {
                    let total = k - i;
                    return Ok(Some((&src[i..i + total], total)));
                }
            }
            j += 1;
        }
    } else {
        // Byte-string with escapes — same as regular string.
        let n = scan_dq_string(src, p)?;
        let total = (p - i) + n;
        Ok(Some((&src[i..i + total], total)))
    }
}

fn scan_dq_string(src: &str, i: usize) -> Result<usize, MinifyError> {
    let bytes = src.as_bytes();
    debug_assert_eq!(bytes[i], b'"');
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => {
                j += 2;
            }
            b'"' => return Ok(j + 1 - i),
            _ => {
                j += 1;
            }
        }
    }
    Err(MinifyError::new("unterminated string literal"))
}

fn scan_quote(src: &str, i: usize) -> Result<(QuoteKind, usize), MinifyError> {
    let bytes = src.as_bytes();
    debug_assert_eq!(bytes[i], b'\'');
    // Lookahead: if `'<single_char>'` or `'\<escape>'` it's a char
    // literal. Otherwise it's a lifetime: `'<ident>`.
    // Heuristic: scan ident chars after `'`, then check for closing `'`.
    let after = i + 1;
    if after >= bytes.len() {
        return Err(MinifyError::new("unterminated `'`"));
    }
    // Escape-led char literal: `'\…'`
    if bytes[after] == b'\\' {
        let mut j = after + 1;
        // Common escapes: '\n','\t','\\','\'','\"','\0','\xNN','\u{…}'
        if j >= bytes.len() {
            return Err(MinifyError::new("unterminated char escape"));
        }
        let esc = bytes[j];
        j += 1;
        if esc == b'x' {
            j = j.saturating_add(2).min(bytes.len()); // two hex digits
        } else if esc == b'u' && peek(bytes, j) == Some(b'{') {
            // skip until matching '}'
            j += 1;
            while j < bytes.len() && bytes[j] != b'}' {
                j += 1;
            }
            if j < bytes.len() {
                j += 1;
            }
        }
        if peek(bytes, j) != Some(b'\'') {
            return Err(MinifyError::new("malformed char literal"));
        }
        return Ok((QuoteKind::Char, j + 1 - i));
    }
    // Otherwise: read ident chars.
    let id_start = after;
    let mut j = id_start;
    while j < bytes.len() && is_id_continue(char_at(src, j)) {
        j += char_at(src, j).len_utf8();
    }
    // If stopped at a closing quote, it's a char literal `'X…'`.
    // Otherwise it's a lifetime.
    if j < bytes.len() && bytes[j] == b'\'' {
        // Char literal — but only if exactly one char between the quotes
        // (since multi-ident is invalid). Tokenize as char regardless: it
        // emits verbatim.
        return Ok((QuoteKind::Char, j + 1 - i));
    }
    // Special-case: an empty `''` is invalid; report.
    if j == id_start {
        // Single non-ident char, no closing quote → not valid Rust.
        // Try scanning a single utf-8 char and see if next is `'`.
        let cl = char_at(src, j).len_utf8();
        if peek(bytes, j + cl) == Some(b'\'') {
            return Ok((QuoteKind::Char, j + cl + 1 - i));
        }
        return Err(MinifyError::new("malformed `'` token"));
    }
    Ok((QuoteKind::Lifetime, j - i))
}

fn is_word_start(src: &str, i: usize) -> bool {
    let c = char_at(src, i);
    c.is_alphabetic() || c == '_' || c.is_ascii_digit()
}

fn is_id_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn scan_word(src: &str, i: usize) -> usize {
    let mut j = i;
    let bytes = src.as_bytes();
    let len = bytes.len();
    while j < len {
        let c = char_at(src, j);
        if c.is_alphanumeric() || c == '_' {
            j += c.len_utf8();
            continue;
        }
        // Numeric literals can have `.` (1.5), `e`/`E` exponent (already
        // handled via alnum), and `_` separator (handled). For our purposes
        // we keep `1.5` as one Word — but tokenizing `.` separately is also
        // fine because `1` then `.` then `5` reassembles via the no-space
        // rule (1.5 has prev=`1` next=`.` no_space, then prev=`.` next=`5`
        // no_space). However `..` (range) and `1.0` collide: `1.0..5.0`
        // would render `1.0..5.0` either way. Safer to treat the digit run
        // and decimal as one token.
        if c == '.' && j > i {
            // Only consume the `.` if followed by a digit (so `1.5` stays
            // word, but `1..5` produces Word(1) then Punct(..) Word(5)).
            let next = peek(bytes, j + 1);
            if matches!(next, Some(b'0'..=b'9')) {
                j += 1;
                continue;
            }
        }
        break;
    }
    j - i
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

    fn min_keep(s: &str) -> String {
        minify(
            s,
            &MinifyOptions {
                keep_comments: true,
            },
        )
        .unwrap()
        .body
    }

    #[test]
    fn basic_function() {
        let src = "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let out = min(src);
        assert_eq!(out, "fn add(a:i32,b:i32)->i32{a+b}");
    }

    #[test]
    fn strips_line_comments() {
        let src = "fn x() {\n    // hi\n    1\n}\n";
        let out = min(src);
        assert_eq!(out, "fn x(){1}");
    }

    #[test]
    fn strips_doc_comments() {
        // Doc comments are line comments to us; stripped by default.
        let src = "/// docs go here\nfn x() {}\n";
        let out = min(src);
        assert_eq!(out, "fn x(){}");
    }

    #[test]
    fn nested_block_comment_stripped() {
        let src = "fn x() { /* outer /* inner */ outer */ 1 }";
        let out = min(src);
        assert_eq!(out, "fn x(){1}");
    }

    #[test]
    fn keep_comments_converts_line_to_block() {
        let src = "fn x() {\n    // hello\n    1\n}\n";
        let r = minify(
            src,
            &MinifyOptions {
                keep_comments: true,
            },
        )
        .unwrap();
        assert!(r.body.contains("/* hello*/"));
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn keep_comments_preserves_block_comment() {
        let src = "fn x() { /* hello */ 1 }";
        let out = min_keep(src);
        assert!(out.contains("/* hello */"));
    }

    #[test]
    fn raw_string_simple() {
        let src = r#"let s = r"hello";"#;
        let out = min(src);
        assert_eq!(out, r#"let s=r"hello";"#);
    }

    #[test]
    fn raw_string_with_hashes() {
        // Source: `let s = r##"con"tains"##;` (raw strings have no
        // backslash escapes — the inner `"` is a literal char).
        let src = "let s = r##\"con\"tains\"##;";
        let out = min(src);
        assert!(out.contains("r##\"con\"tains\"##"), "got: {}", out);
    }

    #[test]
    fn byte_string() {
        let src = r#"let s = b"\xff\x00";"#;
        let out = min(src);
        assert_eq!(out, r#"let s=b"\xff\x00";"#);
    }

    #[test]
    fn raw_byte_string() {
        let src = r#"let s = br"raw bytes";"#;
        let out = min(src);
        assert!(out.contains(r#"br"raw bytes""#));
    }

    #[test]
    fn lifetime_preserved() {
        let src = "fn foo<'a>(x: &'a str) -> &'a str { x }";
        let out = min(src);
        assert_eq!(out, "fn foo<'a>(x:&'a str)->&'a str{x}");
    }

    #[test]
    fn static_lifetime() {
        let src = "let s: &'static str = \"hi\";";
        let out = min(src);
        assert_eq!(out, "let s:&'static str=\"hi\";");
    }

    #[test]
    fn char_literal() {
        let src = "let c = 'a'; let d = '\\n'; let e = '\\u{1F600}';";
        let out = min(src);
        assert!(out.contains("'a'"));
        assert!(out.contains("'\\n'"));
        assert!(out.contains("'\\u{1F600}'"));
    }

    #[test]
    fn byte_char() {
        let src = "let c = b'a';";
        let out = min(src);
        assert_eq!(out, "let c=b'a';");
    }

    #[test]
    fn underscored_number() {
        let src = "let n = 1_000_000;";
        let out = min(src);
        assert_eq!(out, "let n=1_000_000;");
    }

    #[test]
    fn hex_number_with_suffix() {
        let src = "let n = 0xFF_u32;";
        let out = min(src);
        assert_eq!(out, "let n=0xFF_u32;");
    }

    #[test]
    fn float_literal() {
        let src = "let f = 1.5e10;";
        let out = min(src);
        assert_eq!(out, "let f=1.5e10;");
    }

    #[test]
    fn double_colon_preserved() {
        let src = "use std::collections::HashMap;";
        let out = min(src);
        assert_eq!(out, "use std::collections::HashMap;");
    }

    #[test]
    fn arrow_preserved() {
        let src = "fn x() -> i32 { 0 }";
        let out = min(src);
        assert_eq!(out, "fn x()->i32{0}");
    }

    #[test]
    fn fat_arrow_preserved() {
        let src = "match x { 1 => true, _ => false }";
        let out = min(src);
        assert_eq!(out, "match x{1=>true,_=>false}");
    }

    #[test]
    fn unicode_identifier() {
        let src = "let π = 3.14;";
        let out = min(src);
        assert_eq!(out, "let π=3.14;");
    }

    #[test]
    fn range_operator() {
        let src = "let r = 1..5;";
        let out = min(src);
        assert_eq!(out, "let r=1..5;");
    }

    #[test]
    fn unterminated_string_errors() {
        let src = "let s = \"unterminated";
        assert!(minify(src, &MinifyOptions::default()).is_err());
    }

    #[test]
    fn unterminated_block_comment_errors() {
        let src = "fn x() { /* no end";
        assert!(minify(src, &MinifyOptions::default()).is_err());
    }

    #[test]
    fn nested_block_comment_unbalanced_errors() {
        let src = "fn x() { /* /* */ }";
        assert!(minify(src, &MinifyOptions::default()).is_err());
    }
}
