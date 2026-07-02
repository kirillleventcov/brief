use crate::ast::{Inline, ShortArgs};
use crate::diag::{Code, Diagnostic};
use crate::shortcode::ArgValue;
use crate::span::Span;
use std::collections::BTreeMap;

pub fn parse_inline(line: &str, base: u32) -> (Vec<Inline>, Vec<Diagnostic>) {
    let mut p = Parser {
        src: line,
        base,
        pos: 0,
        diags: Vec::new(),
    };
    let nodes = p.parse_until(None);
    (nodes, p.diags)
}

struct Parser<'a> {
    src: &'a str,
    base: u32,
    pos: usize,
    diags: Vec<Diagnostic>,
}

impl<'a> Parser<'a> {
    fn span(&self, start: usize, len: usize) -> Span {
        Span::new(self.base as usize + start, len)
    }

    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    fn parse_until(&mut self, terminator: Option<u8>) -> Vec<Inline> {
        let mut out: Vec<Inline> = Vec::new();
        let mut text_start = self.pos;

        while let Some(c) = self.peek() {
            if Some(c) == terminator {
                break;
            }
            match c {
                b'\\' => {
                    self.flush_text(&mut out, text_start);
                    if let Some(esc_char) = self.src[self.pos + 1..].chars().next() {
                        let w = esc_char.len_utf8();
                        let s = self.span(self.pos, 1 + w);
                        out.push(Inline::Text {
                            value: esc_char.to_string(),
                            span: s,
                        });
                        self.pos += 1 + w;
                    } else {
                        self.pos += 1;
                    }
                    text_start = self.pos;
                }
                b'`' => {
                    self.flush_text(&mut out, text_start);
                    self.parse_code(&mut out);
                    text_start = self.pos;
                }
                b'@' => {
                    self.flush_text(&mut out, text_start);
                    if !self.try_parse_shortcode(&mut out) {
                        out.push(Inline::Text {
                            value: "@".to_string(),
                            span: self.span(self.pos, 1),
                        });
                        self.pos += 1;
                    }
                    text_start = self.pos;
                }
                b'*' | b'_' | b'+' | b'~' if self.is_open_marker() => {
                    self.flush_text(&mut out, text_start);
                    self.parse_emph(&mut out, c);
                    text_start = self.pos;
                }
                _ => {
                    // Advance by full UTF-8 char width so `pos` stays on
                    // a char boundary; otherwise a later sigil would slice
                    // through a multibyte sequence.
                    let w = self.src[self.pos..]
                        .chars()
                        .next()
                        .map_or(1, |c| c.len_utf8());
                    self.pos += w;
                }
            }
        }
        self.flush_text(&mut out, text_start);
        out
    }

    fn flush_text(&self, out: &mut Vec<Inline>, start: usize) {
        if start < self.pos {
            let value = self.src[start..self.pos].to_string();
            out.push(Inline::Text {
                value,
                span: self.span(start, self.pos - start),
            });
        }
    }

    fn is_open_marker(&self) -> bool {
        is_open_marker_at(self.src.as_bytes(), self.pos)
    }

    fn is_close_marker(&self, marker: u8) -> bool {
        let bytes = self.src.as_bytes();
        let pos = self.pos;
        if bytes.get(pos) != Some(&marker) {
            return false;
        }
        if bytes.get(pos + 1) == Some(&marker) {
            return false;
        }
        let prev = if pos == 0 { None } else { Some(bytes[pos - 1]) };
        let next = bytes.get(pos + 1).copied();
        let prev_ok = matches!(prev, Some(b) if b != b' ');
        let next_ok = match next {
            None => true,
            Some(b' ') => true,
            Some(b) => is_inline_sigil(b) || is_punct(b),
        };
        prev_ok && next_ok
    }

    fn parse_emph(&mut self, out: &mut Vec<Inline>, marker: u8) {
        let start = self.pos;
        self.pos += 1;
        let inner_start = self.pos;
        let mut content: Vec<Inline> = Vec::new();
        let mut text_start = inner_start;
        let mut closed = false;

        while let Some(c) = self.peek() {
            if c == marker && self.is_close_marker(marker) {
                if text_start < self.pos {
                    content.push(Inline::Text {
                        value: self.src[text_start..self.pos].to_string(),
                        span: self.span(text_start, self.pos - text_start),
                    });
                }
                self.pos += 1;
                closed = true;
                break;
            }
            if c == marker {
                self.diags.push(
                    Diagnostic::new(Code::EmphasisSameMarker, self.span(self.pos, 1))
                        .label("inner emphasis re-uses the same marker")
                        .help("use a different emphasis marker for the inner span"),
                );
                self.pos += 1;
                continue;
            }
            match c {
                b'\\' => {
                    if text_start < self.pos {
                        content.push(Inline::Text {
                            value: self.src[text_start..self.pos].to_string(),
                            span: self.span(text_start, self.pos - text_start),
                        });
                    }
                    if let Some(esc_char) = self.src[self.pos + 1..].chars().next() {
                        let w = esc_char.len_utf8();
                        content.push(Inline::Text {
                            value: esc_char.to_string(),
                            span: self.span(self.pos, 1 + w),
                        });
                        self.pos += 1 + w;
                    } else {
                        self.pos += 1;
                    }
                    text_start = self.pos;
                }
                b'`' => {
                    if text_start < self.pos {
                        content.push(Inline::Text {
                            value: self.src[text_start..self.pos].to_string(),
                            span: self.span(text_start, self.pos - text_start),
                        });
                    }
                    self.parse_code(&mut content);
                    text_start = self.pos;
                }
                b'@' => {
                    if text_start < self.pos {
                        content.push(Inline::Text {
                            value: self.src[text_start..self.pos].to_string(),
                            span: self.span(text_start, self.pos - text_start),
                        });
                    }
                    if !self.try_parse_shortcode(&mut content) {
                        content.push(Inline::Text {
                            value: "@".to_string(),
                            span: self.span(self.pos, 1),
                        });
                        self.pos += 1;
                    }
                    text_start = self.pos;
                }
                b'*' | b'_' | b'+' | b'~' if c != marker && self.is_open_marker() => {
                    if text_start < self.pos {
                        content.push(Inline::Text {
                            value: self.src[text_start..self.pos].to_string(),
                            span: self.span(text_start, self.pos - text_start),
                        });
                    }
                    self.parse_emph(&mut content, c);
                    text_start = self.pos;
                }
                _ => {
                    let w = self.src[self.pos..]
                        .chars()
                        .next()
                        .map_or(1, |c| c.len_utf8());
                    self.pos += w;
                }
            }
        }
        if !closed {
            self.diags.push(
                Diagnostic::new(Code::UnterminatedEmph, self.span(start, 1))
                    .label(format!("opened with `{}`", marker as char)),
            );
        }
        let span = self.span(start, self.pos - start);
        let node = match marker {
            b'*' => Inline::Bold { content, span },
            b'_' => Inline::Italic { content, span },
            b'+' => Inline::Underline { content, span },
            b'~' => Inline::Strike { content, span },
            _ => unreachable!(),
        };
        out.push(node);
    }

    fn parse_code(&mut self, out: &mut Vec<Inline>) {
        let start = self.pos;
        let mut ticks = 0;
        while self.peek() == Some(b'`') && ticks < 2 {
            self.pos += 1;
            ticks += 1;
        }
        if self.peek() == Some(b'`') {
            out.push(Inline::Text {
                value: self.src[start..self.pos].to_string(),
                span: self.span(start, self.pos - start),
            });
            return;
        }
        let body_start = self.pos;
        let needle = if ticks == 1 {
            "`".to_string()
        } else {
            "``".to_string()
        };
        let rest = &self.src[body_start..];
        if let Some(rel) = rest.find(&needle) {
            let body = &self.src[body_start..body_start + rel];
            self.pos = body_start + rel + needle.len();
            out.push(Inline::InlineCode {
                value: body.to_string(),
                span: self.span(start, self.pos - start),
            });
        } else {
            self.diags.push(Diagnostic::new(
                Code::UnterminatedCode,
                self.span(start, ticks),
            ));
            out.push(Inline::Text {
                value: self.src[start..self.pos].to_string(),
                span: self.span(start, self.pos - start),
            });
        }
    }

    fn try_parse_shortcode(&mut self, out: &mut Vec<Inline>) -> bool {
        let saved = self.pos;
        if self.peek() != Some(b'@') {
            return false;
        }
        let mut cursor = self.pos + 1;
        let bytes = self.src.as_bytes();
        if bytes
            .get(cursor)
            .map(|b| !b.is_ascii_alphabetic())
            .unwrap_or(true)
        {
            return false;
        }
        let name_start = cursor;
        while let Some(&b) = bytes.get(cursor) {
            if b.is_ascii_alphanumeric() || b == b'-' {
                cursor += 1;
            } else {
                break;
            }
        }
        let name = self.src[name_start..cursor].to_string();
        let mut args = ShortArgs::default();
        if bytes.get(cursor) == Some(&b'(') {
            match parse_args(self.src, &mut cursor) {
                Ok(a) => args = a,
                Err(d) => {
                    self.diags.push(d.label("in inline shortcode"));
                    self.pos = cursor;
                    out.push(Inline::Text {
                        value: self.src[saved..self.pos].to_string(),
                        span: self.span(saved, self.pos - saved),
                    });
                    return true;
                }
            }
        }
        self.pos = cursor;
        let mut content = None;
        if self.peek() == Some(b'[') {
            self.pos += 1;
            let inner = self.parse_until(Some(b']'));
            if self.peek() == Some(b']') {
                self.pos += 1;
            }
            content = Some(inner);

            // Markdown-link sugar: `@name[text](url)` -- the trailing parens
            // hold a single raw-string positional argument. This is the only
            // place where a URL-like value escapes the strict arg grammar.
            // Parens inside the URL are allowed when balanced (Wikipedia
            // disambiguation URLs); the closing delimiter is the first `)`
            // that isn't matched by a `(` inside the URL.
            if self.peek() == Some(b'(') {
                self.pos += 1;
                let url_start = self.pos;
                let mut open_parens = 0usize;
                while let Some(b) = self.peek() {
                    match b {
                        b'(' => open_parens += 1,
                        b')' => {
                            if open_parens == 0 {
                                break;
                            }
                            open_parens -= 1;
                        }
                        _ => {}
                    }
                    self.pos += 1;
                }
                let url = self.src[url_start..self.pos].to_string();
                if self.peek() == Some(b')') {
                    self.pos += 1;
                } else {
                    self.diags.push(
                        Diagnostic::new(
                            Code::BadArgSyntax,
                            self.span(url_start, self.pos - url_start),
                        )
                        .label("`(url)` is never closed")
                        .help("percent-encode unmatched parentheses in the URL as %28 / %29"),
                    );
                }
                args.positional.push(ArgValue::Str(url));
            }
        }
        let span = self.span(saved, self.pos - saved);
        out.push(Inline::Shortcode {
            name,
            args,
            content,
            span,
        });
        true
    }
}

pub(crate) fn is_inline_sigil(b: u8) -> bool {
    matches!(b, b'*' | b'_' | b'+' | b'~' | b'`' | b'@' | b'[' | b']')
}

pub(crate) fn is_punct(b: u8) -> bool {
    matches!(
        b,
        b'.' | b',' | b';' | b':' | b'!' | b'?' | b')' | b'(' | b'"' | b'\'' | b'-' | b'/'
    )
}

/// `true` if `bytes[pos]` (which must be one of `*`/`_`/`+`/`~`) would
/// open an emphasis span at this position under Brief's flanking rules.
/// Shared between the inline parser and the Markdown→Brief converter so
/// the two cannot drift.
pub(crate) fn is_open_marker_at(bytes: &[u8], pos: usize) -> bool {
    let marker = match bytes.get(pos) {
        Some(&b @ (b'*' | b'_' | b'+' | b'~')) => b,
        _ => return false,
    };
    let prev = if pos == 0 { None } else { Some(bytes[pos - 1]) };
    let next = bytes.get(pos + 1).copied();
    if next == Some(marker) || prev == Some(marker) {
        return false;
    }
    let prev_ok = match prev {
        None => true,
        Some(b' ') => true,
        Some(b) if is_inline_sigil(b) => true,
        Some(b) if is_punct(b) => true,
        _ => false,
    };
    let next_ok = matches!(next, Some(b) if b != b' ' && b != marker);
    prev_ok && next_ok
}

pub fn parse_args(src: &str, cursor: &mut usize) -> Result<ShortArgs, Diagnostic> {
    let bytes = src.as_bytes();
    if bytes.get(*cursor) != Some(&b'(') {
        return Ok(ShortArgs::default());
    }
    *cursor += 1;
    let mut args = ShortArgs::default();
    let mut keys_seen: BTreeMap<String, ()> = BTreeMap::new();
    skip_ws(src, cursor);
    if bytes.get(*cursor) == Some(&b')') {
        *cursor += 1;
        return Ok(args);
    }
    loop {
        skip_ws(src, cursor);
        let arg_start = *cursor;
        let saved = *cursor;
        if let Some(name) = read_ident(src, cursor) {
            skip_ws(src, cursor);
            if bytes.get(*cursor) == Some(&b':') {
                *cursor += 1;
                skip_ws(src, cursor);
                let v = read_value(src, cursor)
                    .ok_or_else(|| Diagnostic::new(Code::BadArgSyntax, Span::new(*cursor, 1)))?;
                if keys_seen.insert(name.clone(), ()).is_some() {
                    return Err(Diagnostic::new(
                        Code::DuplicateKwarg,
                        Span::new(arg_start, name.len()),
                    ));
                }
                args.keyword.insert(name, v);
            } else {
                *cursor = saved;
                let v = read_value(src, cursor)
                    .ok_or_else(|| Diagnostic::new(Code::BadArgSyntax, Span::new(*cursor, 1)))?;
                args.positional.push(v);
            }
        } else {
            let v = read_value(src, cursor)
                .ok_or_else(|| Diagnostic::new(Code::BadArgSyntax, Span::new(*cursor, 1)))?;
            args.positional.push(v);
        }
        skip_ws(src, cursor);
        match bytes.get(*cursor) {
            Some(&b',') => {
                *cursor += 1;
                continue;
            }
            Some(&b')') => {
                *cursor += 1;
                break;
            }
            _ => return Err(Diagnostic::new(Code::BadArgSyntax, Span::new(*cursor, 1))),
        }
    }
    Ok(args)
}

fn skip_ws(src: &str, cursor: &mut usize) {
    while src.as_bytes().get(*cursor) == Some(&b' ') {
        *cursor += 1;
    }
}

fn read_ident(src: &str, cursor: &mut usize) -> Option<String> {
    let bytes = src.as_bytes();
    let start = *cursor;
    let first = *bytes.get(start)?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let mut end = start + 1;
    while let Some(&b) = bytes.get(end) {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
            end += 1;
        } else {
            break;
        }
    }
    *cursor = end;
    Some(src[start..end].to_string())
}

fn read_value(src: &str, cursor: &mut usize) -> Option<ArgValue> {
    skip_ws(src, cursor);
    let bytes = src.as_bytes();
    let start = *cursor;
    match bytes.get(start)? {
        b'"' => {
            *cursor += 1;
            let mut s = String::new();
            while *cursor < bytes.len() {
                let b = bytes[*cursor];
                if b == b'"' {
                    *cursor += 1;
                    return Some(ArgValue::Str(s));
                }
                if b == b'\\' {
                    if let Some(c) = src[*cursor + 1..].chars().next() {
                        s.push(c);
                        *cursor += 1 + c.len_utf8();
                        continue;
                    }
                    // dangling backslash at EOF: treat as literal
                    s.push('\\');
                    *cursor += 1;
                    continue;
                }
                // Take one full char so cursor stays UTF-8 aligned and the
                // produced string preserves multibyte content correctly.
                let c = src[*cursor..].chars().next().expect("cursor < len");
                s.push(c);
                *cursor += c.len_utf8();
            }
            None
        }
        b'[' => {
            *cursor += 1;
            let mut arr: Vec<ArgValue> = Vec::new();
            skip_ws(src, cursor);
            if bytes.get(*cursor) == Some(&b']') {
                *cursor += 1;
                return Some(ArgValue::Array(arr));
            }
            loop {
                let v = read_value(src, cursor)?;
                arr.push(v);
                skip_ws(src, cursor);
                match bytes.get(*cursor) {
                    Some(&b',') => {
                        *cursor += 1;
                        skip_ws(src, cursor);
                    }
                    Some(&b']') => {
                        *cursor += 1;
                        return Some(ArgValue::Array(arr));
                    }
                    _ => return None,
                }
            }
        }
        c if c.is_ascii_digit() || *c == b'-' => {
            let mut end = start;
            if bytes[end] == b'-' {
                end += 1;
            }
            while let Some(&b) = bytes.get(end) {
                if b.is_ascii_digit() {
                    end += 1;
                } else {
                    break;
                }
            }
            let n: i64 = src[start..end].parse().ok()?;
            *cursor = end;
            Some(ArgValue::Int(n))
        }
        c if c.is_ascii_alphabetic() => {
            let id = read_ident(src, cursor)?;
            Some(ArgValue::Ident(id))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> (Vec<Inline>, Vec<Diagnostic>) {
        parse_inline(s, 0)
    }

    #[test]
    fn plain_text() {
        let (n, d) = parse("hello world");
        assert!(d.is_empty());
        assert_eq!(n.len(), 1);
        if let Inline::Text { value, .. } = &n[0] {
            assert_eq!(value, "hello world");
        } else {
            panic!();
        }
    }

    #[test]
    fn bold() {
        let (n, d) = parse("a *bold* b");
        assert!(d.is_empty(), "{:?}", d);
        assert!(matches!(n[1], Inline::Bold { .. }));
    }

    #[test]
    fn snake_case_is_literal() {
        let (n, d) = parse("snake_case_name");
        assert!(d.is_empty());
        assert_eq!(n.len(), 1);
        assert!(matches!(n[0], Inline::Text { .. }));
    }

    #[test]
    fn nested_same_marker_errors() {
        let (_, d) = parse("*outer *inner* outer*");
        assert!(d.iter().any(|x| x.code == Code::EmphasisSameMarker));
    }

    #[test]
    fn link_url_with_balanced_parens() {
        let (n, d) =
            parse("@link[rust](https://en.wikipedia.org/wiki/Rust_(programming_language)) x");
        assert!(d.is_empty(), "{:?}", d);
        if let Inline::Shortcode { args, .. } = &n[0] {
            assert_eq!(
                args.positional[0].as_str(),
                Some("https://en.wikipedia.org/wiki/Rust_(programming_language)")
            );
        } else {
            panic!("expected shortcode, got {:?}", n[0]);
        }
        if let Inline::Text { value, .. } = &n[1] {
            assert_eq!(value, " x");
        } else {
            panic!("tail must survive as text");
        }
    }

    #[test]
    fn link_url_unterminated_paren_is_b0406() {
        let (_, d) = parse("@link[x](https://e.com/a(b now");
        assert!(d.iter().any(|x| x.code == Code::BadArgSyntax), "{:?}", d);
    }

    #[test]
    fn inline_code() {
        let (n, d) = parse("use `printf` here");
        assert!(d.is_empty());
        assert!(matches!(n[1], Inline::InlineCode { .. }));
    }

    #[test]
    fn double_backtick_code_with_backtick() {
        let (n, d) = parse("``a ` b``");
        assert!(d.is_empty());
        if let Inline::InlineCode { value, .. } = &n[0] {
            assert_eq!(value, "a ` b");
        } else {
            panic!();
        }
    }

    #[test]
    fn shortcode_inline() {
        let (n, d) = parse("see @link[here](https://x)");
        assert!(d.is_empty(), "{:?}", d);
        assert!(matches!(n.last().unwrap(), Inline::Shortcode { .. }));
    }

    #[test]
    fn escape_emphasis() {
        let (n, d) = parse(r"\*literal\*");
        assert!(d.is_empty());
        let joined: String = n
            .iter()
            .filter_map(|x| {
                if let Inline::Text { value, .. } = x {
                    Some(value.clone())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(joined, "*literal*");
    }

    #[test]
    fn double_marker_not_emphasis() {
        let (n, _d) = parse("**no**");
        assert!(!matches!(n[0], Inline::Bold { .. }));
    }

    #[test]
    fn escape_before_multibyte_char() {
        // Regression: escaping `\é` used to advance two bytes past `\` and
        // land mid-codepoint, panicking the next slice. The escape must
        // consume the full UTF-8 char.
        let (n, d) = parse("a \\é b");
        assert!(d.is_empty(), "{:?}", d);
        let joined: String = n
            .iter()
            .filter_map(|x| {
                if let Inline::Text { value, .. } = x {
                    Some(value.clone())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(joined, "a é b");
    }

    #[test]
    fn multibyte_text_then_emph() {
        // Regression: the byte-walking text scanner must land on a char
        // boundary before any sigil, even when text is multibyte.
        let (n, d) = parse("日本 *bold*");
        assert!(d.is_empty(), "{:?}", d);
        assert!(matches!(n.last().unwrap(), Inline::Bold { .. }));
    }

    #[test]
    fn arg_string_preserves_multibyte() {
        // Regression: read_value used to push raw bytes as Latin-1 chars,
        // corrupting multibyte content inside a string argument.
        let mut cursor = 0usize;
        let s = "(label: \"日本 🦀\")";
        let args = parse_args(s, &mut cursor).unwrap();
        if let ArgValue::Str(v) = args.keyword.get("label").unwrap() {
            assert_eq!(v, "日本 🦀");
        } else {
            panic!();
        }
    }

    #[test]
    fn escape_at_end_of_input() {
        // A trailing `\\` with nothing to escape must not panic.
        let (_, _d) = parse("trailing\\");
    }
}
