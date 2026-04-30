//! Java minifier.
//!
//! Distinguishing features vs the generic C-family base:
//!
//! - Text blocks `"""…"""` (Java 13+) — content can span multiple lines
//!   and contain unescaped `"`. We treat the whole literal as one
//!   StrLit, scanning until the next `"""` not preceded by `\`.
//! - Annotations: `@Override`, `@SuppressWarnings("foo")`. The `@` is
//!   not a comment marker — it's part of an identifier-like token. We
//!   emit `@<ident>` as one Word.
//! - Block comments do **not** nest (unlike Rust).
//!
//! Strategy: aggressive (Strategy A). Java requires explicit semicolons,
//! so newlines can be stripped freely.

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
            while j + 1 < bytes.len() {
                if bytes[j] == b'*' && bytes[j + 1] == b'/' {
                    let body = &src[body_start..j];
                    out.push(Token::new(TokenKind::BlockComment(body)));
                    i = j + 2;
                    break;
                }
                j += 1;
            }
            if i <= body_start {
                return Err(MinifyError::new("unterminated /* */ block comment"));
            }
            continue;
        }
        // Text block `"""…"""`.
        if c == b'"' && peek(bytes, i + 1) == Some(b'"') && peek(bytes, i + 2) == Some(b'"') {
            let start = i;
            let mut j = i + 3;
            loop {
                if j + 2 >= bytes.len() {
                    return Err(MinifyError::new("unterminated text block"));
                }
                if bytes[j] == b'"' && bytes[j + 1] == b'"' && bytes[j + 2] == b'"' {
                    // Unescaped triple-quote? It's the close. Java allows
                    // `\"""` for an in-text triple-quote sequence.
                    let escaped = j > start + 3 && bytes[j - 1] == b'\\';
                    if !escaped {
                        j += 3;
                        break;
                    }
                }
                j += 1;
            }
            out.push(Token::new(TokenKind::StrLit(&src[start..j])));
            i = j;
            continue;
        }
        if c == b'"' {
            let n = scan_dq_string(src, i)?;
            out.push(Token::new(TokenKind::StrLit(&src[i..i + n])));
            i += n;
            continue;
        }
        if c == b'\'' {
            let n = scan_char_literal(src, i)?;
            out.push(Token::new(TokenKind::StrLit(&src[i..i + n])));
            i += n;
            continue;
        }
        // Annotation `@<ident>`. The `@` is part of the token so it stays
        // bound to the ident on emit (no risk of `@ Override` rendering).
        if c == b'@' && peek(bytes, i + 1).map_or(false, is_ident_start_byte) {
            let mut j = i + 1;
            while j < bytes.len() && is_ident_continue_byte(bytes[j]) {
                j += 1;
            }
            out.push(Token::new(TokenKind::Word(&src[i..j])));
            i = j;
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
            b'\\' => {
                j += 2;
            }
            b'"' => return Ok(j + 1 - i),
            b'\n' => return Err(MinifyError::new("newline in string literal")),
            _ => j += 1,
        }
    }
    Err(MinifyError::new("unterminated string literal"))
}

fn scan_char_literal(src: &str, i: usize) -> Result<usize, MinifyError> {
    let bytes = src.as_bytes();
    debug_assert_eq!(bytes[i], b'\'');
    let mut j = i + 1;
    if j >= bytes.len() {
        return Err(MinifyError::new("unterminated char literal"));
    }
    if bytes[j] == b'\\' {
        j += 2;
        // simple escapes; numeric escapes (\uXXXX, octal) handled greedily
        while j < bytes.len() && bytes[j] != b'\'' && bytes[j] != b'\n' {
            j += 1;
        }
    } else {
        // single Unicode char
        j += char_at(src, j).len_utf8();
    }
    if peek(bytes, j) != Some(b'\'') {
        return Err(MinifyError::new("malformed char literal"));
    }
    Ok(j + 1 - i)
}

fn is_ident_start_byte(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$'
}
fn is_ident_continue_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn is_word_start(src: &str, i: usize) -> bool {
    let c = char_at(src, i);
    c.is_alphabetic() || c == '_' || c == '$' || c.is_ascii_digit()
}

fn scan_word(src: &str, i: usize) -> usize {
    let bytes = src.as_bytes();
    let mut j = i;
    while j < bytes.len() {
        let c = char_at(src, j);
        if c.is_alphanumeric() || c == '_' || c == '$' {
            j += c.len_utf8();
            continue;
        }
        if c == '.' {
            // 1.5, 1.5e10
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
    if matches!(three, "<<=" | ">>=" | ">>>" | "..." | "->>") {
        return 3;
    }
    if matches!(
        two,
        "->" | "=="
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
    fn class_with_method() {
        let src = "public class Foo {\n    public int add(int a, int b) {\n        return a + b;\n    }\n}\n";
        let out = min(src);
        assert_eq!(
            out,
            "public class Foo{public int add(int a,int b){return a+b;}}"
        );
    }

    #[test]
    fn strips_line_comment() {
        let src = "// hi\nint x;\n";
        let out = min(src);
        assert_eq!(out, "int x;");
    }

    #[test]
    fn strips_block_comment() {
        let src = "/* hi */ int x;\n";
        let out = min(src);
        assert_eq!(out, "int x;");
    }

    #[test]
    fn annotation_preserved() {
        let src = "@Override public void f() {}";
        let out = min(src);
        assert_eq!(out, "@Override public void f(){}");
    }

    #[test]
    fn annotation_with_args() {
        let src = "@SuppressWarnings(\"unchecked\") void f() {}";
        let out = min(src);
        assert_eq!(out, "@SuppressWarnings(\"unchecked\")void f(){}");
    }

    #[test]
    fn text_block_preserved() {
        let src = "String s = \"\"\"\nhello\nworld\n\"\"\";\n";
        let out = min(src);
        assert!(out.contains("\"\"\"\nhello\nworld\n\"\"\""));
    }

    #[test]
    fn string_with_escape() {
        let src = "String s = \"a\\\"b\";";
        let out = min(src);
        assert_eq!(out, "String s=\"a\\\"b\";");
    }

    #[test]
    fn char_literal() {
        let src = "char c = 'a';";
        let out = min(src);
        assert_eq!(out, "char c='a';");
    }

    #[test]
    fn keep_comments_converts_line() {
        let src = "// hi\nint x;\n";
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
    fn dollar_in_identifier() {
        let src = "int $x = 1;";
        let out = min(src);
        assert_eq!(out, "int $x=1;");
    }

    #[test]
    fn unterminated_string_errors() {
        assert!(minify("String s = \"oops", &MinifyOptions::default()).is_err());
    }

    #[test]
    fn unterminated_block_comment_errors() {
        assert!(minify("/* unterminated", &MinifyOptions::default()).is_err());
    }

    #[test]
    fn lambda_arrow() {
        let src = "x -> x + 1";
        let out = min(src);
        assert_eq!(out, "x->x+1");
    }

    #[test]
    fn diamond_operator() {
        let src = "List<Integer> xs = new ArrayList<>();";
        let out = min(src);
        // `>` to `x` (punct → word) needs no space; `>>=`/`<<` collisions
        // are handled at lex time so we don't emit `>>` accidentally.
        assert_eq!(out, "List<Integer>xs=new ArrayList<>();");
    }
}
