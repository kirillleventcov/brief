//! JavaScript / TypeScript minifier.
//!
//! The JS lexer is the most complex of the v0.3 set:
//!
//! - **Template literals** `` `…${expr}…${expr}…` ``. The body is
//!   literal except inside `${…}` interpolations, which contain
//!   arbitrary JS code (and may themselves contain template literals,
//!   recursively). We track interpolation by scanning for `${`, then
//!   counting `{`/`}` until the brace balance returns to zero.
//! - **Regex literals** `/pattern/flags`. Lexically ambiguous with
//!   division. We disambiguate via the previous-significant-token
//!   heuristic: regex iff the previous non-whitespace, non-comment
//!   token was a punctuator (other than `)`/`]`/`++`/`--`) or one
//!   of the expression-position keywords (`return`, `typeof`, `in`,
//!   `of`, `delete`, `void`, `new`, `throw`, `await`, `yield`,
//!   `instanceof`, `case`, `do`, `else`). A previous `}` is ambiguous
//!   on its own: a statement-block `}` is followed by regex, an
//!   object-literal (or arrow-body) `}` by division. We track every
//!   `{`'s kind on a stack, classified by the token before it.
//! - **ASI**. JavaScript can implicitly insert `;` at end of certain
//!   lines. Stripping such newlines without inserting an explicit `;`
//!   changes semantics (`return\n{x:1}` returns undefined; `return{x:1}`
//!   returns the object). Without a real parser we **preserve newlines**
//!   verbatim and trust the engine's ASI rules.
//! - TypeScript adds type syntax (`x: T`, `<T>`, `as T`) but the
//!   tokenization is unchanged from JS, so the same lexer handles both.
//!
//! Strategy: conservative (Strategy B). Newlines preserved.

use super::c_common::{Token, TokenKind, emit_conservative};
use super::{MinifyError, MinifyOptions, MinifyOutput};

pub fn minify(source: &str, opts: &MinifyOptions) -> Result<MinifyOutput, MinifyError> {
    let toks = tokenize(source)?;
    emit_conservative(&toks, opts.keep_comments)
}

/// What a `{` opened — decides whether the matching `}` ends a statement
/// (regex may follow) or an expression (division may follow).
#[derive(Clone, Copy, PartialEq)]
enum BraceKind {
    Block,
    Object,
}

fn tokenize(src: &str) -> Result<Vec<Token<'_>>, MinifyError> {
    let bytes = src.as_bytes();
    let mut out: Vec<Token<'_>> = Vec::new();
    let mut brace_stack: Vec<BraceKind> = Vec::new();
    // Kind of the most recent `}` token. Only consulted when that `}` is
    // still the previous significant token, so one slot is enough.
    let mut last_close = BraceKind::Block;
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
        // Regex disambiguation: a `/` may start a regex or be division.
        if c == b'/' && regex_is_expected(&out, last_close) {
            let n = scan_regex(src, i)?;
            out.push(Token::new(TokenKind::Regex(&src[i..i + n])));
            i += n;
            continue;
        }
        if c == b'"' || c == b'\'' {
            let n = scan_quoted_string(src, i, c)?;
            out.push(Token::new(TokenKind::StrLit(&src[i..i + n])));
            i += n;
            continue;
        }
        if c == b'`' {
            let n = scan_template(src, i)?;
            out.push(Token::new(TokenKind::Template(&src[i..i + n])));
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
        let punct = &src[i..i + n];
        if punct == "{" {
            brace_stack.push(if open_brace_is_object(&out) {
                BraceKind::Object
            } else {
                BraceKind::Block
            });
        } else if punct == "}" {
            // Unbalanced `}` (mid-snippet input): assume statement position.
            last_close = brace_stack.pop().unwrap_or(BraceKind::Block);
        }
        out.push(Token::new(TokenKind::Punct(punct)));
        i += n;
    }
    Ok(out)
}

/// Is a `{` here an object literal (expression) rather than a statement
/// block? Also groups arrow bodies (`=> {`) with expressions, because their
/// closing `}` ends an expression either way.
fn open_brace_is_object(prev_tokens: &[Token<'_>]) -> bool {
    for tok in prev_tokens.iter().rev() {
        match &tok.kind {
            TokenKind::LineComment(_) | TokenKind::BlockComment(_) | TokenKind::Newline => continue,
            TokenKind::Word(s) => {
                return matches!(
                    *s,
                    "return"
                        | "typeof"
                        | "in"
                        | "of"
                        | "delete"
                        | "void"
                        | "new"
                        | "throw"
                        | "await"
                        | "yield"
                        | "instanceof"
                        | "case"
                );
            }
            TokenKind::Punct(s) => {
                // After a closed group, another brace, `;`, or postfix
                // operator, a `{` starts a statement block (function/if/for
                // bodies arrive via `) {`). Anywhere else — after `=`, `(`,
                // `,`, `:`, `[`, binary operators, `=>` — it's an expression.
                return !matches!(*s, ")" | "]" | "}" | "{" | ";" | "++" | "--");
            }
            TokenKind::StrLit(_)
            | TokenKind::Template(_)
            | TokenKind::Regex(_)
            | TokenKind::Preproc(_) => return false,
        }
    }
    // Start of source: statement position.
    false
}

/// Heuristic: should a `/` here start a regex literal? Yes iff the previous
/// "significant" token (skipping whitespace/comments/newlines) is one
/// after which an expression is expected. `last_close` is the kind of the
/// most recent `}`, used when that `}` is the previous significant token.
fn regex_is_expected(prev_tokens: &[Token<'_>], last_close: BraceKind) -> bool {
    // Scan backward past comments/newlines.
    for tok in prev_tokens.iter().rev() {
        match &tok.kind {
            TokenKind::LineComment(_) | TokenKind::BlockComment(_) | TokenKind::Newline => continue,
            TokenKind::Word(s) => {
                return matches!(
                    *s,
                    "return"
                        | "typeof"
                        | "in"
                        | "of"
                        | "delete"
                        | "void"
                        | "new"
                        | "throw"
                        | "await"
                        | "yield"
                        | "instanceof"
                        | "case"
                        | "do"
                        | "else"
                );
            }
            TokenKind::Punct(s) => {
                // A statement-block `}` is followed by a new statement, which
                // may open with a regex; an object-literal/arrow-body `}`
                // ends an expression, so `/` is division.
                if *s == "}" {
                    return last_close == BraceKind::Block;
                }
                // After `)`, `]`, `++`, `--`, an expression has ended;
                // `/` is division. Anything else, expect regex.
                return !matches!(*s, ")" | "]" | "++" | "--");
            }
            TokenKind::StrLit(_)
            | TokenKind::Template(_)
            | TokenKind::Regex(_)
            | TokenKind::Preproc(_) => return false,
        }
    }
    // No previous significant token: top of source. An expression at the
    // very start could begin with a regex literal (rare but legal).
    true
}

fn scan_regex(src: &str, i: usize) -> Result<usize, MinifyError> {
    let bytes = src.as_bytes();
    debug_assert_eq!(bytes[i], b'/');
    let mut j = i + 1;
    let mut in_class = false;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => {
                j += 2;
                continue;
            }
            b'[' => {
                in_class = true;
                j += 1;
            }
            b']' if in_class => {
                in_class = false;
                j += 1;
            }
            b'/' if !in_class => {
                // skip closing /, then flags (Latin letters)
                j += 1;
                while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                    j += 1;
                }
                return Ok(j - i);
            }
            b'\n' => return Err(MinifyError::new("newline in regex literal")),
            _ => j += 1,
        }
    }
    Err(MinifyError::new("unterminated regex literal"))
}

fn scan_quoted_string(src: &str, i: usize, quote: u8) -> Result<usize, MinifyError> {
    let bytes = src.as_bytes();
    debug_assert_eq!(bytes[i], quote);
    let mut j = i + 1;
    while j < bytes.len() {
        if bytes[j] == b'\\' {
            // Line continuation: `\<nl>` is allowed in JS strings.
            if peek(bytes, j + 1) == Some(b'\n') {
                j += 2;
                continue;
            }
            j += 2;
            continue;
        }
        if bytes[j] == quote {
            return Ok(j + 1 - i);
        }
        if bytes[j] == b'\n' {
            return Err(MinifyError::new("newline in string literal"));
        }
        j += 1;
    }
    Err(MinifyError::new("unterminated string literal"))
}

fn scan_template(src: &str, i: usize) -> Result<usize, MinifyError> {
    let bytes = src.as_bytes();
    debug_assert_eq!(bytes[i], b'`');
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => {
                j += 2;
            }
            b'`' => return Ok(j + 1 - i),
            b'$' if peek(bytes, j + 1) == Some(b'{') => {
                // Skip `${`, then content until matching `}` (counting
                // braces, accounting for nested templates/strings).
                j += 2;
                let mut depth = 1usize;
                while j < bytes.len() && depth > 0 {
                    match bytes[j] {
                        b'{' => {
                            depth += 1;
                            j += 1;
                        }
                        b'}' => {
                            depth -= 1;
                            j += 1;
                        }
                        b'`' => {
                            // Nested template literal — recurse.
                            let inner = scan_template(src, j)?;
                            j += inner;
                        }
                        b'"' | b'\'' => {
                            let q = bytes[j];
                            j += scan_quoted_string(src, j, q)?;
                        }
                        b'/' if peek(bytes, j + 1) == Some(b'/') => {
                            while j < bytes.len() && bytes[j] != b'\n' {
                                j += 1;
                            }
                        }
                        b'/' if peek(bytes, j + 1) == Some(b'*') => {
                            j += 2;
                            while j + 1 < bytes.len() && !(bytes[j] == b'*' && bytes[j + 1] == b'/')
                            {
                                j += 1;
                            }
                            if j + 1 >= bytes.len() {
                                return Err(MinifyError::new("unterminated /* */ inside template"));
                            }
                            j += 2;
                        }
                        b'\\' => {
                            j += 2;
                        }
                        _ => j += 1,
                    }
                }
                if depth != 0 {
                    return Err(MinifyError::new("unterminated `${…}` in template"));
                }
            }
            _ => j += 1,
        }
    }
    Err(MinifyError::new("unterminated template literal"))
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
            // Decimal: 1.5; scientific: 1e3 (handled by alnum already).
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
    let four = bytes
        .get(i..i + 4)
        .map(|s| std::str::from_utf8(s).unwrap_or(""))
        .unwrap_or("");
    let three = bytes
        .get(i..i + 3)
        .map(|s| std::str::from_utf8(s).unwrap_or(""))
        .unwrap_or("");
    let two = bytes
        .get(i..i + 2)
        .map(|s| std::str::from_utf8(s).unwrap_or(""))
        .unwrap_or("");
    if matches!(four, ">>>=") {
        return 4;
    }
    if matches!(
        three,
        "===" | "!==" | "..." | ">>>" | "**=" | "<<=" | ">>=" | "??="
    ) {
        return 3;
    }
    if matches!(
        two,
        "=>" | "=="
            | "!="
            | "<="
            | ">="
            | "&&"
            | "||"
            | "??"
            | "?."
            | "++"
            | "--"
            | "<<"
            | ">>"
            | "**"
            | "+="
            | "-="
            | "*="
            | "/="
            | "%="
            | "&="
            | "|="
            | "^="
            | "&&="
            | "||="
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
        let src = "function add(a, b) {\n    return a + b;\n}\n";
        let out = min(src);
        // Newlines preserved (ASI), horizontal ws stripped.
        assert_eq!(out, "function add(a,b){\nreturn a+b;\n}\n");
    }

    #[test]
    fn strips_line_comment() {
        let src = "// hi\nlet x = 1;\n";
        let out = min(src);
        assert_eq!(out, "\nlet x=1;\n");
    }

    #[test]
    fn strips_block_comment_inline() {
        let src = "let x = /* y */ 1;\n";
        let out = min(src);
        assert_eq!(out, "let x=1;\n");
    }

    #[test]
    fn template_literal() {
        let src = "const s = `hello, ${name}!`;\n";
        let out = min(src);
        assert!(out.contains("`hello, ${name}!`"), "got: {}", out);
    }

    #[test]
    fn nested_template() {
        let src = "const s = `a${`b${c}d`}e`;\n";
        let out = min(src);
        assert!(out.contains("`a${`b${c}d`}e`"), "got: {}", out);
    }

    #[test]
    fn template_with_string_in_interpolation() {
        let src = "const s = `${\"hi\"}`;\n";
        let out = min(src);
        assert!(out.contains("`${\"hi\"}`"), "got: {}", out);
    }

    #[test]
    fn regex_literal() {
        let src = "const re = /[a-z]+/gi;\n";
        let out = min(src);
        assert_eq!(out, "const re=/[a-z]+/gi;\n");
    }

    #[test]
    fn regex_after_return() {
        let src = "function f() { return /\\d+/.test(x); }\n";
        let out = min(src);
        assert!(out.contains("/\\d+/"), "got: {}", out);
    }

    #[test]
    fn division_after_value() {
        let src = "const x = a / b;\n";
        let out = min(src);
        assert_eq!(out, "const x=a/b;\n");
    }

    #[test]
    fn division_after_paren() {
        let src = "const x = (a + b) / c;\n";
        let out = min(src);
        assert_eq!(out, "const x=(a+b)/c;\n");
    }

    #[test]
    fn return_then_object_preserves_newline() {
        // ASI hazard: `return\n{x:1}` returns undefined. Stripping the
        // newline would change behavior. Conservative emitter preserves.
        let src = "function f() {\n    return\n    {x: 1};\n}\n";
        let out = min(src);
        assert!(
            out.contains("return\n"),
            "newline preserved after return: {:?}",
            out
        );
    }

    #[test]
    fn arrow_function() {
        let src = "const f = (x) => x + 1;\n";
        let out = min(src);
        assert_eq!(out, "const f=(x)=>x+1;\n");
    }

    #[test]
    fn nullish_coalescing() {
        let src = "const x = a ?? b;\n";
        let out = min(src);
        assert_eq!(out, "const x=a??b;\n");
    }

    #[test]
    fn optional_chaining() {
        let src = "const x = obj?.prop;\n";
        let out = min(src);
        assert_eq!(out, "const x=obj?.prop;\n");
    }

    #[test]
    fn strict_equality() {
        let src = "if (a === b) {}\n";
        let out = min(src);
        assert_eq!(out, "if(a===b){}\n");
    }

    #[test]
    fn typescript_type_annotation() {
        let src = "function f(x: number): string { return String(x); }\n";
        let out = min(src);
        // Newline-free single-line input → single-line output.
        assert_eq!(out, "function f(x:number):string{return String(x);}\n");
    }

    #[test]
    fn typescript_generic() {
        let src = "function f<T>(x: T): T { return x; }\n";
        let out = min(src);
        assert_eq!(out, "function f<T>(x:T):T{return x;}\n");
    }

    #[test]
    fn double_quoted_string_with_escape() {
        let src = "const s = \"a\\\"b\";\n";
        let out = min(src);
        assert_eq!(out, "const s=\"a\\\"b\";\n");
    }

    #[test]
    fn dollar_in_identifier() {
        let src = "const $foo = 1;\n";
        let out = min(src);
        assert_eq!(out, "const $foo=1;\n");
    }

    #[test]
    fn keep_comments_converts_line() {
        let src = "// hi\nlet x = 1;\n";
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
    fn unterminated_string() {
        assert!(minify("const s = \"oops", &MinifyOptions::default()).is_err());
    }

    #[test]
    fn unterminated_template() {
        assert!(minify("const s = `oops", &MinifyOptions::default()).is_err());
    }

    #[test]
    fn unterminated_regex() {
        assert!(minify("const r = /oops", &MinifyOptions::default()).is_err());
    }

    #[test]
    fn regex_after_statement_block_brace() {
        // A `}` closing a statement block may be followed by a regex; the
        // whitespace inside it must survive.
        let src = "if (c) { g() } /foo  bar/.exec(s)\n";
        let out = min(src);
        assert!(out.contains("/foo  bar/"), "got: {}", out);
    }

    #[test]
    fn regex_after_else_block_brace() {
        let src = "if (c) { g() } else { h() } /foo  bar/.exec(s)\n";
        let out = min(src);
        assert!(out.contains("/foo  bar/"), "got: {}", out);
    }

    #[test]
    fn division_after_object_literal_brace() {
        // A `}` closing an object literal ends an expression; `/` here is
        // division and must not swallow code as a regex body.
        let src = "const x = {a: 1} / b / c;\n";
        let out = min(src);
        assert_eq!(out, "const x={a:1}/b/c;\n");
    }

    #[test]
    fn nested_blocks_and_object_braces() {
        let src = "function f() { const o = {a: 1}; if (o) { g() } } /re  x/.test(s)\n";
        let out = min(src);
        assert!(out.contains("/re  x/"), "got: {}", out);
    }

    #[test]
    fn regex_with_class() {
        let src = "const r = /[/]/g;\n";
        let out = min(src);
        assert!(out.contains("/[/]/g"), "got: {}", out);
    }

    #[test]
    fn regex_at_start_of_file() {
        let src = "/abc/.test(s)\n";
        let out = min(src);
        assert!(out.starts_with("/abc/"), "got: {}", out);
    }
}
