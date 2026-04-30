//! Go minifier.
//!
//! Distinguishing features:
//!
//! - Backtick raw strings `` `…` `` — no escapes; can span lines.
//! - Block comments do **not** nest.
//! - **Automatic semicolon insertion** (ASI). Go's lexer inserts a
//!   `;` at the end of any line whose final token is one of:
//!   identifier, integer/float/imaginary/rune/string literal, the
//!   keywords `break`/`continue`/`fallthrough`/`return`, the operators
//!   `++`/`--`, or one of `)`/`]`/`}`. Stripping such newlines without
//!   inserting a `;` would change semantics. Without a real parser we
//!   take the safe-by-construction approach: **preserve newlines**.
//!
//! Strategy: conservative (Strategy B). Horizontal whitespace and
//! comments are stripped; newlines are preserved verbatim.

use super::c_common::{Token, TokenKind, emit_conservative};
use super::{MinifyError, MinifyOptions, MinifyOutput};

pub fn minify(source: &str, opts: &MinifyOptions) -> Result<MinifyOutput, MinifyError> {
    let toks = tokenize(source)?;
    emit_conservative(&toks, opts.keep_comments)
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
            let body_start = i + 2;
            let mut j = body_start;
            let mut found = false;
            while j + 1 < bytes.len() {
                if bytes[j] == b'*' && bytes[j + 1] == b'/' {
                    found = true;
                    break;
                }
                j += 1;
            }
            if !found {
                return Err(MinifyError::new("unterminated /* */ block comment"));
            }
            out.push(Token::new(TokenKind::BlockComment(&src[body_start..j])));
            i = j + 2;
            continue;
        }
        if c == b'"' {
            let n = scan_dq_string(src, i)?;
            out.push(Token::new(TokenKind::StrLit(&src[i..i + n])));
            i += n;
            continue;
        }
        if c == b'`' {
            // Backtick raw string — verbatim, no escapes, may span lines.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'`' {
                j += 1;
            }
            if j >= bytes.len() {
                return Err(MinifyError::new("unterminated raw string"));
            }
            out.push(Token::new(TokenKind::StrLit(&src[i..j + 1])));
            i = j + 1;
            continue;
        }
        if c == b'\'' {
            let n = scan_rune(src, i)?;
            out.push(Token::new(TokenKind::StrLit(&src[i..i + n])));
            i += n;
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

fn scan_dq_string(src: &str, i: usize) -> Result<usize, MinifyError> {
    let bytes = src.as_bytes();
    debug_assert_eq!(bytes[i], b'"');
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'"' => return Ok(j + 1 - i),
            b'\n' => return Err(MinifyError::new("newline in interpreted string")),
            _ => j += 1,
        }
    }
    Err(MinifyError::new("unterminated string literal"))
}

fn scan_rune(src: &str, i: usize) -> Result<usize, MinifyError> {
    let bytes = src.as_bytes();
    debug_assert_eq!(bytes[i], b'\'');
    let mut j = i + 1;
    if j >= bytes.len() {
        return Err(MinifyError::new("unterminated rune literal"));
    }
    if bytes[j] == b'\\' {
        j += 2;
        // Greedy until next `'`
        while j < bytes.len() && bytes[j] != b'\'' && bytes[j] != b'\n' {
            j += 1;
        }
    } else {
        j += char_at(src, j).len_utf8();
    }
    if peek(bytes, j) != Some(b'\'') {
        return Err(MinifyError::new("malformed rune literal"));
    }
    Ok(j + 1 - i)
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
    let three = bytes
        .get(i..i + 3)
        .map(|s| std::str::from_utf8(s).unwrap_or(""))
        .unwrap_or("");
    let two = bytes
        .get(i..i + 2)
        .map(|s| std::str::from_utf8(s).unwrap_or(""))
        .unwrap_or("");
    if matches!(three, "<<=" | ">>=" | "..." | "&^=") {
        return 3;
    }
    if matches!(
        two,
        ":=" | "=="
            | "!="
            | "<="
            | ">="
            | "&&"
            | "||"
            | "<<"
            | ">>"
            | "+="
            | "-="
            | "*="
            | "/="
            | "%="
            | "&="
            | "|="
            | "^="
            | "++"
            | "--"
            | "<-"
            | "&^"
    ) {
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
    fn basic_function() {
        let src = "func add(a, b int) int {\n    return a + b\n}\n";
        let out = min(src);
        // Newlines preserved (ASI), horizontal ws stripped.
        assert_eq!(out, "func add(a,b int)int{\nreturn a+b\n}\n");
    }

    #[test]
    fn strips_line_comment() {
        let src = "// hi\nx := 1\n";
        let out = min(src);
        // Newline after stripped comment is collapsed to one trailing \n.
        assert_eq!(out, "\nx:=1\n");
    }

    #[test]
    fn strips_block_comment_inline() {
        let src = "x := /* y */ 1\n";
        let out = min(src);
        assert_eq!(out, "x:=1\n");
    }

    #[test]
    fn backtick_raw_string_multiline() {
        let src = "s := `multi\nline\nstring`\n";
        let out = min(src);
        assert!(out.contains("`multi\nline\nstring`"), "got: {}", out);
    }

    #[test]
    fn rune_literal() {
        let src = "r := 'a'\n";
        let out = min(src);
        assert_eq!(out, "r:='a'\n");
    }

    #[test]
    fn channel_op() {
        let src = "ch <- 1\n";
        let out = min(src);
        assert_eq!(out, "ch<-1\n");
    }

    #[test]
    fn short_var_declaration() {
        let src = "x := 1\n";
        let out = min(src);
        assert_eq!(out, "x:=1\n");
    }

    #[test]
    fn return_then_brace_preserves_newline() {
        // ASI hazard: `return\n{ x }` is `return; { x }`. Stripping the
        // newline would change meaning. We preserve newlines so this is
        // safe even without parsing.
        let src = "return\n{ x }\n";
        let out = min(src);
        assert!(out.contains("return\n"), "newline preserved: {:?}", out);
    }

    #[test]
    fn keep_comments_converts() {
        let src = "// hi\nx := 1\n";
        let r = minify(
            src,
            &MinifyOptions {
                keep_comments: true,
            },
        )
        .unwrap();
        assert!(r.body.contains("/* hi*/"));
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn unterminated_backtick() {
        assert!(minify("s := `nope", &MinifyOptions::default()).is_err());
    }

    #[test]
    fn unterminated_string() {
        assert!(minify("s := \"nope", &MinifyOptions::default()).is_err());
    }
}
