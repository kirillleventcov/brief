//! C and C++ minifier.
//!
//! Distinguishing features:
//!
//! - Preprocessor directives (`#include`, `#define`, …) are
//!   line-sensitive — the C preprocessor reads input line-by-line and
//!   directives must end at a newline (or be continued via `\<nl>`). We
//!   tokenize each `#`-line as a single `Preproc` token and the emitter
//!   places it on its own line.
//! - String prefixes: `L"…"`, `u8"…"`, `u"…"`, `U"…"` (wide / UTF
//!   literals).
//! - C++ raw strings `R"delim(…)delim"` (and prefixed forms like
//!   `LR"delim(…)delim"`, `u8R"…"`, etc.) — only when `is_cpp` is true.
//! - Block comments do **not** nest.
//!
//! Strategy: aggressive (Strategy A) **between** preprocessor lines.
//! Around `#`-lines we still emit a leading and trailing newline.
//!
//! `is_cpp` toggles the C++-only features (raw strings). For C tags
//! (`c`, `h`) it's false; for C++ tags (`cpp`, `c++`, `cc`, `cxx`,
//! `hpp`, `hxx`) it's true.

use super::c_common::{Token, TokenKind};
use super::{MinifyError, MinifyOptions, MinifyOutput, MinifyWarning};

pub fn minify(
    source: &str,
    opts: &MinifyOptions,
    is_cpp: bool,
) -> Result<MinifyOutput, MinifyError> {
    let toks = tokenize(source, is_cpp)?;
    emit(&toks, opts.keep_comments)
}

fn tokenize(src: &str, is_cpp: bool) -> Result<Vec<Token<'_>>, MinifyError> {
    let bytes = src.as_bytes();
    let mut out: Vec<Token<'_>> = Vec::new();
    let mut i = 0usize;
    let mut at_line_start = true;
    while i < bytes.len() {
        let c = bytes[i];
        if matches!(c, b' ' | b'\t' | b'\r') {
            i += 1;
            continue;
        }
        if c == b'\n' {
            out.push(Token::new(TokenKind::Newline));
            i += 1;
            at_line_start = true;
            continue;
        }
        // Preprocessor directive: `#` at start of line (after optional
        // whitespace) — already consumed above. Captures everything up to
        // the newline, plus any backslash-newline continuations.
        if at_line_start && c == b'#' {
            let start = i;
            let mut j = i;
            while j < bytes.len() {
                if bytes[j] == b'\\' && peek(bytes, j + 1) == Some(b'\n') {
                    j += 2;
                    continue;
                }
                if bytes[j] == b'\\'
                    && peek(bytes, j + 1) == Some(b'\r')
                    && peek(bytes, j + 2) == Some(b'\n')
                {
                    j += 3;
                    continue;
                }
                if bytes[j] == b'\n' {
                    break;
                }
                j += 1;
            }
            out.push(Token::new(TokenKind::Preproc(&src[start..j])));
            i = j;
            // Don't consume the newline — let the main loop handle it so
            // the emitter sees a Newline after the Preproc.
            at_line_start = false;
            continue;
        }
        at_line_start = false;
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
        // String / raw-string detection. C++ raw strings use the `R"d(…)d"`
        // form; combinations include `LR`, `uR`, `UR`, `u8R`. Plain strings
        // can be prefixed `L`, `u`, `u8`, `U`.
        if let Some(n) = try_scan_string(src, i, is_cpp)? {
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

fn emit(tokens: &[Token<'_>], keep_comments: bool) -> Result<MinifyOutput, MinifyError> {
    let mut out = String::new();
    let mut warnings: Vec<MinifyWarning> = Vec::new();
    let mut prev_emit_last: Option<char> = None;
    let mut last_was_preproc = false;
    for tok in tokens {
        match &tok.kind {
            TokenKind::Newline => {
                // Newlines are always discarded UNLESS the previous emitted
                // token was a preprocessor line, in which case we keep one
                // terminating newline so `#include <…>` is on its own line.
                if last_was_preproc && !out.ends_with('\n') {
                    out.push('\n');
                    prev_emit_last = None;
                    last_was_preproc = false;
                }
            }
            TokenKind::LineComment(body) => {
                if !keep_comments {
                    continue;
                }
                let block = format!("/*{}*/", body);
                push_with_space(&mut out, &mut prev_emit_last, &block);
                warnings.push(MinifyWarning::LineCommentConverted);
            }
            TokenKind::BlockComment(body) => {
                if !keep_comments {
                    continue;
                }
                let block = format!("/*{}*/", body);
                push_with_space(&mut out, &mut prev_emit_last, &block);
            }
            TokenKind::Word(s)
            | TokenKind::Punct(s)
            | TokenKind::StrLit(s)
            | TokenKind::Template(s)
            | TokenKind::Regex(s) => {
                push_with_space(&mut out, &mut prev_emit_last, s);
                last_was_preproc = false;
            }
            TokenKind::Preproc(s) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(s);
                prev_emit_last = None;
                last_was_preproc = true;
            }
        }
    }
    if last_was_preproc && !out.ends_with('\n') {
        out.push('\n');
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
    use super::c_common::needs_space;
    if let Some(prev) = *prev_emit_last {
        if let Some(next) = s.chars().next() {
            if needs_space(prev, next) {
                out.push(' ');
            }
        }
    }
    out.push_str(s);
    *prev_emit_last = s.chars().next_back();
}

fn try_scan_string(src: &str, i: usize, is_cpp: bool) -> Result<Option<usize>, MinifyError> {
    let bytes = src.as_bytes();
    // Possible prefixes: `L`, `u`, `u8`, `U`. Each may also be combined
    // with `R` for C++ raw strings (`LR`, `uR`, `u8R`, `UR`).
    let mut p = i;
    let mut had_prefix = false;
    // u8 first
    if peek(bytes, p) == Some(b'u') && peek(bytes, p + 1) == Some(b'8') {
        // Could be u8" or u8R" — but only commit if a quote/R follows.
        let after = p + 2;
        if peek(bytes, after) == Some(b'"')
            || (is_cpp && peek(bytes, after) == Some(b'R') && peek(bytes, after + 1) == Some(b'"'))
        {
            p = after;
            had_prefix = true;
        }
    } else if matches!(peek(bytes, p), Some(b'L') | Some(b'u') | Some(b'U')) {
        let after = p + 1;
        if peek(bytes, after) == Some(b'"')
            || (is_cpp && peek(bytes, after) == Some(b'R') && peek(bytes, after + 1) == Some(b'"'))
        {
            p = after;
            had_prefix = true;
        }
    }
    let raw = is_cpp && peek(bytes, p) == Some(b'R') && peek(bytes, p + 1) == Some(b'"');
    if raw {
        // R"delim(…)delim"
        p += 1; // skip R
        debug_assert_eq!(bytes[p], b'"');
        let delim_start = p + 1;
        let mut j = delim_start;
        while j < bytes.len() && bytes[j] != b'(' {
            j += 1;
        }
        if j >= bytes.len() {
            return Err(MinifyError::new("malformed raw string"));
        }
        let delim = &bytes[delim_start..j];
        let body_start = j + 1;
        // find `)delim"`
        let mut k = body_start;
        loop {
            if k >= bytes.len() {
                return Err(MinifyError::new("unterminated raw string"));
            }
            if bytes[k] == b')' && k + 1 + delim.len() < bytes.len() {
                if &bytes[k + 1..k + 1 + delim.len()] == delim
                    && bytes.get(k + 1 + delim.len()) == Some(&b'"')
                {
                    let total = k + 1 + delim.len() + 1 - i;
                    return Ok(Some(total));
                }
            }
            k += 1;
        }
    }
    if peek(bytes, p) == Some(b'"') {
        let n = scan_dq_string(src, p)?;
        return Ok(Some(p + n - i));
    }
    if had_prefix {
        // We thought we had a prefix but no quote followed — back out.
        return Ok(None);
    }
    Ok(None)
}

fn scan_dq_string(src: &str, i: usize) -> Result<usize, MinifyError> {
    let bytes = src.as_bytes();
    debug_assert_eq!(bytes[i], b'"');
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
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
    while j < bytes.len() {
        if bytes[j] == b'\\' {
            j += 2;
            continue;
        }
        if bytes[j] == b'\'' {
            return Ok(j + 1 - i);
        }
        if bytes[j] == b'\n' {
            return Err(MinifyError::new("newline in char literal"));
        }
        j += 1;
    }
    Err(MinifyError::new("unterminated char literal"))
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
    if matches!(three, "<<=" | ">>=" | "..." | "->*") {
        return 3;
    }
    if matches!(
        two,
        "->" | "::"
            | "=="
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
            | ".*"
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

    fn min_c(s: &str) -> String {
        minify(s, &MinifyOptions::default(), false).unwrap().body
    }
    fn min_cpp(s: &str) -> String {
        minify(s, &MinifyOptions::default(), true).unwrap().body
    }

    #[test]
    fn c_basic() {
        let src = "int main() {\n    return 0;\n}\n";
        assert_eq!(min_c(src), "int main(){return 0;}");
    }

    #[test]
    fn c_preprocessor_kept_on_own_line() {
        let src = "#include <stdio.h>\nint main() { return 0; }\n";
        let out = min_c(src);
        assert!(
            out.starts_with("#include <stdio.h>\n"),
            "preproc on own line: {:?}",
            out
        );
        assert!(out.contains("int main(){return 0;}"));
    }

    #[test]
    fn c_multiple_preprocessor_lines() {
        let src = "#include <stdio.h>\n#include <stdlib.h>\nint x;\n";
        let out = min_c(src);
        assert_eq!(out, "#include <stdio.h>\n#include <stdlib.h>\nint x;");
    }

    #[test]
    fn c_define_with_continuation() {
        let src = "#define FOO(x) \\\n    do { x; } while (0)\nint y = 1;\n";
        let out = min_c(src);
        // The whole `#define` line, including its `\<nl>` continuations,
        // is one Preproc token; the next `int y = 1;` is on a new line.
        assert!(out.starts_with("#define FOO(x) \\\n    do { x; } while (0)\n"));
        assert!(out.ends_with("int y=1;"));
    }

    #[test]
    fn c_strips_line_comment() {
        let src = "// hi\nint x;\n";
        assert_eq!(min_c(src), "int x;");
    }

    #[test]
    fn c_strips_block_comment() {
        let src = "/* hi */ int x;\n";
        assert_eq!(min_c(src), "int x;");
    }

    #[test]
    fn cpp_template_double_close() {
        // C++11+ parsers correctly disambiguate `>>` in template contexts
        // from the right-shift operator, so collapsing the source `>> ` to
        // `>>` is safe. The lexer emits `>>` as one Punct because the
        // source already had them adjacent.
        let src = "vector<vector<int>> v;";
        let out = min_cpp(src);
        assert_eq!(out, "vector<vector<int>>v;");
    }

    #[test]
    fn cpp_template_with_space_at_close() {
        // If the source separated the closing `> >` with a space, the
        // lexer sees them as two separate Puncts; the emitter then injects
        // a space because `>` `>` is in the dangerous-pair table.
        let src = "vector<vector<int> > v;";
        let out = min_cpp(src);
        assert!(out.contains("> >"), "got: {}", out);
    }

    #[test]
    fn cpp_raw_string() {
        let src = r#"const char* s = R"x(hi)x";"#;
        let out = min_cpp(src);
        assert!(out.contains(r#"R"x(hi)x""#), "got: {}", out);
    }

    #[test]
    fn cpp_wide_string() {
        let src = "const wchar_t* s = L\"hi\";";
        let out = min_cpp(src);
        assert!(out.contains("L\"hi\""));
    }

    #[test]
    fn cpp_u8_string() {
        let src = "const char* s = u8\"hi\";";
        let out = min_cpp(src);
        assert!(out.contains("u8\"hi\""));
    }

    #[test]
    fn cpp_arrow_member() {
        let src = "p->x = 1;";
        let out = min_cpp(src);
        assert_eq!(out, "p->x=1;");
    }

    #[test]
    fn cpp_scope_resolution() {
        let src = "std::string s;";
        let out = min_cpp(src);
        assert_eq!(out, "std::string s;");
    }

    #[test]
    fn c_keep_comments() {
        let src = "// hi\nint x;\n";
        let r = minify(
            src,
            &MinifyOptions {
                keep_comments: true,
            },
            false,
        )
        .unwrap();
        assert!(r.body.starts_with("/* hi*/"));
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn c_unterminated_block_comment() {
        assert!(minify("/* unterminated", &MinifyOptions::default(), false).is_err());
    }

    #[test]
    fn c_unterminated_string() {
        assert!(minify("char* s = \"oops", &MinifyOptions::default(), false).is_err());
    }
}
