use crate::ast::{Inline, ShortArgs};
use crate::diag::{Code, Diagnostic};
use crate::parser::MAX_NESTING_DEPTH;
use crate::shortcode::ArgValue;
use crate::span::Span;
use std::collections::BTreeMap;

/// One source line's share of a string built by joining several lines
/// (paragraph or definition continuations with a single space): bytes from
/// `joined` onward came from source offset `source`, up to the next
/// segment. Without it, spans drift by every newline, indent and trailing
/// space the join removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Seg {
    pub(crate) joined: u32,
    pub(crate) source: u32,
}

pub fn parse_inline(line: &str, base: u32) -> (Vec<Inline>, Vec<Diagnostic>) {
    parse_inline_joined(
        line,
        0,
        &[Seg {
            joined: 0,
            source: base,
        }],
    )
}

/// Source offset of byte `at` of the joined string `segs` describes. A
/// segment boundary is both the end of one segment and the start of the
/// next; `as_end` picks the former so a span ending at a line break stops
/// at that line's text instead of jumping to the next line.
fn joined_to_source(segs: &[Seg], at: usize, as_end: bool) -> usize {
    let i = segs
        .partition_point(|s| {
            let j = s.joined as usize;
            if as_end { j < at } else { j <= at }
        })
        .saturating_sub(1);
    segs[i].source as usize + (at - segs[i].joined as usize)
}

/// Parse `text`, which starts `origin` bytes into the joined string that
/// `segs` describes.
pub(crate) fn parse_inline_joined(
    text: &str,
    origin: usize,
    segs: &[Seg],
) -> (Vec<Inline>, Vec<Diagnostic>) {
    let mut p = Parser {
        src: text,
        origin,
        segs,
        pos: 0,
        diags: Vec::new(),
        open_emph: 0,
        depth: 0,
        depth_reported: false,
        comments_scanned: false,
        comment_stars: Vec::new(),
    };
    let (nodes, _) = p.parse_seq(None, None);
    (nodes, p.diags)
}

struct Parser<'a> {
    src: &'a str,
    origin: usize,
    segs: &'a [Seg],
    pos: usize,
    diags: Vec<Diagnostic>,
    /// Emphasis kinds open around `pos` within the current shortcode body,
    /// one bit per marker. Same-kind nesting is illegal, so at most four
    /// emphasis frames are ever on the stack between shortcode bodies.
    open_emph: u8,
    /// Shortcode bodies (`@name[...]`) open around `pos`. Each recurses on
    /// the native stack, so they are capped like block nesting.
    depth: usize,
    depth_reported: bool,
    comments_scanned: bool,
    /// The `*` of each `*/` already reported as B0307. It stays literal so
    /// it doesn't also open (or clash with) an emphasis span and bury the
    /// real error under a B0207 or B0204.
    comment_stars: Vec<usize>,
}

/// Bytes the inline scanner must stop at: escape, code span, shortcode,
/// the four emphasis markers, and `]` (the only terminator ever passed to
/// `parse_seq`). Everything else — including UTF-8 continuation bytes,
/// which are all >= 0x80 and never collide with these ASCII values — is
/// skipped one byte at a time without decoding. The scanner only ever
/// *stops* on ASCII, so `pos` always lands on a char boundary.
const SPECIAL: [bool; 256] = {
    let mut t = [false; 256];
    t[b'\\' as usize] = true;
    t[b'`' as usize] = true;
    t[b'@' as usize] = true;
    t[b'*' as usize] = true;
    t[b'_' as usize] = true;
    t[b'+' as usize] = true;
    t[b'~' as usize] = true;
    t[b']' as usize] = true;
    t
};

/// Index of the first byte at or after `from` that the scanner must stop
/// at, or `bytes.len()`. Kept out of line so the skip over ordinary text
/// stays a tight loop however the code around its call site is laid out.
#[inline(never)]
fn next_special(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|&b| SPECIAL[b as usize])
        .map_or(bytes.len(), |i| from + i)
}

fn emph_bit(marker: u8) -> u8 {
    match marker {
        b'*' => 1,
        b'_' => 2,
        b'+' => 4,
        _ => 8,
    }
}

impl<'a> Parser<'a> {
    fn span(&self, start: usize, len: usize) -> Span {
        // Single-line input, by far the common case: a plain offset.
        if let [seg] = self.segs {
            let at = seg.source as usize + self.origin + start - seg.joined as usize;
            return Span::new(at, len);
        }
        let a = joined_to_source(self.segs, self.origin + start, false);
        if len == 0 {
            return Span::new(a, 0);
        }
        let b = joined_to_source(self.segs, self.origin + start + len, true);
        Span::new(a, b - a)
    }

    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    /// Parse until end of input, the `terminator` byte (left unconsumed),
    /// or a valid closer for `closer`, the marker of the emphasis span this
    /// run sits in (consumed). Returns the nodes and whether the closer
    /// was found.
    fn parse_seq(&mut self, terminator: Option<u8>, closer: Option<u8>) -> (Vec<Inline>, bool) {
        let mut out: Vec<Inline> = Vec::new();
        let mut text_start = self.pos;
        let bytes = self.src.as_bytes();

        loop {
            self.pos = next_special(bytes, self.pos);
            let Some(&c) = bytes.get(self.pos) else {
                break;
            };
            if c == b'*' && !self.comments_scanned && self.pos > 0 && bytes[self.pos - 1] == b'/' {
                // First `/*` in the input: look for misplaced comments now,
                // before the scan reaches a `*/` it would read as emphasis.
                // Deferring the check keeps it off the common path.
                self.comments_scanned = true;
                self.check_inline_comments();
            }
            if Some(c) == closer {
                if self.is_close_marker(c) {
                    self.flush_text(&mut out, text_start);
                    self.pos += 1;
                    return (out, true);
                }
                if !self.is_comment_star() {
                    self.same_marker(self.pos);
                }
                self.pos += 1;
                continue;
            }
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
                b'*' | b'_' | b'+' | b'~' if self.is_open_marker() && !self.is_comment_star() => {
                    if self.open_emph & emph_bit(c) != 0 {
                        // An enclosing span of this kind is still open.
                        // Refusing to recurse here is also what bounds the
                        // emphasis stack at four frames.
                        self.same_marker(self.pos);
                        self.pos += 1;
                        continue;
                    }
                    self.flush_text(&mut out, text_start);
                    self.parse_emph(&mut out, c, terminator);
                    text_start = self.pos;
                }
                _ => {
                    // A special byte that didn't start a construct (`]`
                    // with no terminator armed, or a marker that isn't an
                    // opener). All special bytes are ASCII, so +1 keeps
                    // `pos` on a char boundary.
                    self.pos += 1;
                }
            }
        }
        self.flush_text(&mut out, text_start);
        (out, false)
    }

    fn is_comment_star(&self) -> bool {
        !self.comment_stars.is_empty() && self.comment_stars.contains(&self.pos)
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

    fn same_marker(&mut self, at: usize) {
        self.diags.push(
            Diagnostic::new(Code::EmphasisSameMarker, self.span(at, 1))
                .label("inner emphasis re-uses the same marker")
                .help("use a different emphasis marker for the inner span"),
        );
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

    fn parse_emph(&mut self, out: &mut Vec<Inline>, marker: u8, terminator: Option<u8>) {
        let start = self.pos;
        self.pos += 1;
        let bit = emph_bit(marker);
        self.open_emph |= bit;
        let (content, closed) = self.parse_seq(terminator, Some(marker));
        self.open_emph &= !bit;
        if !closed {
            self.diags.push(
                Diagnostic::new(Code::UnterminatedEmph, self.span(start, 1))
                    .label(format!("opened with `{}`", marker as char)),
            );
            // The marker reverts to literal text and everything parsed
            // after it is kept as siblings, so an unclosed span can never
            // take the rest of the line down with it.
            out.push(Inline::Text {
                value: (marker as char).to_string(),
                span: self.span(start, 1),
            });
            out.extend(content);
            return;
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
            match parse_args(self.src, &mut cursor, 0) {
                Ok(a) => args = a,
                Err(mut d) => {
                    // Offsets came back relative to `src`; map them like
                    // every other span so joined lines don't drift.
                    d.span = self.span(d.span.start as usize, d.span.len as usize);
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
            if name == "math" {
                content = Some(self.parse_math_body());
            } else if self.depth >= MAX_NESTING_DEPTH {
                if !self.depth_reported {
                    self.depth_reported = true;
                    self.diags.push(
                        Diagnostic::new(Code::NestingTooDeep, self.span(saved, 1)).label(format!(
                            "inline shortcodes nest deeper than {} levels",
                            MAX_NESTING_DEPTH
                        )),
                    );
                }
                // Leave the `[` to the enclosing run as plain text rather
                // than recursing again.
            } else {
                let open = self.pos;
                self.pos += 1;
                self.depth += 1;
                // Emphasis inside a body is independent of emphasis around
                // the shortcode; the body's own depth is what's bounded.
                let outer_emph = std::mem::take(&mut self.open_emph);
                let (inner, _) = self.parse_seq(Some(b']'), None);
                self.open_emph = outer_emph;
                self.depth -= 1;
                if self.peek() == Some(b']') {
                    self.pos += 1;
                } else {
                    self.diags.push(
                        Diagnostic::new(Code::BadArgSyntax, self.span(open, 1))
                            .label(format!("`@{}[` is never closed", name)),
                    );
                }
                content = Some(inner);
            }

            // Markdown-link sugar: `@name[text](url)` -- the trailing parens
            // hold a single raw-string positional argument. This is the only
            // place where a URL-like value escapes the strict arg grammar.
            // Parens inside the URL are allowed when balanced (Wikipedia
            // disambiguation URLs); the closing delimiter is the first `)`
            // that isn't matched by a `(` inside the URL.
            if content.is_some() && self.peek() == Some(b'(') {
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

    /// `@math[...]` bodies are verbatim TeX, like inline code: no escapes
    /// (TeX owns the backslash) and no nested markup. TeX uses square
    /// brackets itself (`\sqrt[3]{x}`, `[0, 1]`), so the body runs to the
    /// `]` that balances the opening `[`. An unbalanced body is an error
    /// rather than a silently truncated formula; an unmatched bracket is
    /// written `\lbrack` / `\rbrack`.
    fn parse_math_body(&mut self) -> Vec<Inline> {
        let open = self.pos;
        let body_start = open + 1;
        let end = match math_body_end(self.src.as_bytes(), body_start) {
            Some(end) => {
                self.pos = end + 1;
                end
            }
            None => {
                self.diags.push(
                    Diagnostic::new(Code::BadArgSyntax, self.span(open, 1))
                        .label("`@math[` is never closed")
                        .help(
                            "brackets inside `@math[...]` must balance; write an unmatched one as `\\lbrack` or `\\rbrack`",
                        ),
                );
                self.pos = self.src.len();
                self.pos
            }
        };
        if body_start == end {
            return Vec::new();
        }
        vec![Inline::Text {
            value: self.src[body_start..end].to_string(),
            span: self.span(body_start, end - body_start),
        }]
    }

    /// Brief strips `/* */` only when it opens a line, so a comment written
    /// after content would otherwise be published as prose. Flags a
    /// `/* ` ... ` */` pair in one inline run: the `/*` at the start or
    /// after a space and followed by a space, the `*/` after a space, both
    /// outside code spans, math bodies and escapes. Globs and paths such as
    /// `rm /*.tmp` or `a/*b` never match.
    fn check_inline_comments(&mut self) {
        let bytes = self.src.as_bytes();
        let mut open: Option<usize> = None;
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2,
                b'`' => i = skip_code_span(bytes, i),
                b'@' if bytes[i + 1..].starts_with(b"math[") => {
                    i = math_body_end(bytes, i + 6).map_or(bytes.len(), |e| e + 1);
                }
                b'/' if bytes.get(i + 1) == Some(&b'*')
                    && bytes.get(i + 2) == Some(&b' ')
                    && (i == 0 || bytes[i - 1] == b' ') =>
                {
                    open = Some(i);
                    i += 2;
                }
                b'*' if i > 0 && bytes[i - 1] == b' ' && bytes.get(i + 1) == Some(&b'/') => {
                    if let Some(o) = open.take() {
                        self.comment_stars.push(i);
                        self.diags.push(
                            Diagnostic::new(Code::InlineBlockComment, self.span(o, i + 2 - o))
                                .label("comment shares a line with content and would be rendered")
                                .help("move the comment to its own line, or write `\\/*` if the text is literal"),
                        );
                    }
                    i += 2;
                }
                _ => i += 1,
            }
        }
    }
}

/// End of a `@math[` body starting at `from`: the `]` that balances the
/// opening bracket.
fn math_body_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &b) in bytes.get(from..)?.iter().enumerate() {
        match b {
            b'[' => depth += 1,
            b']' if depth == 0 => return Some(from + i),
            b']' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Position just past the code span (or literal backtick run) at `start`,
/// following `Parser::parse_code`'s rules exactly.
fn skip_code_span(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    let mut ticks = 0;
    while bytes.get(i) == Some(&b'`') && ticks < 2 {
        i += 1;
        ticks += 1;
    }
    if bytes.get(i) == Some(&b'`') {
        return i;
    }
    let needle = &b"``"[..ticks];
    match bytes[i..].windows(ticks).position(|w| w == needle) {
        Some(rel) => i + rel + ticks,
        None => i,
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

/// Parse a `(...)` argument list starting at `*cursor`. `src` is the text
/// the cursor indexes and `base` is its offset in the source, so diagnostic
/// spans come back absolute.
pub fn parse_args(src: &str, cursor: &mut usize, base: u32) -> Result<ShortArgs, Diagnostic> {
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
                let v = read_value(src, cursor).ok_or_else(|| {
                    Diagnostic::new(Code::BadArgSyntax, char_span(src, *cursor, base))
                })?;
                if keys_seen.insert(name.clone(), ()).is_some() {
                    return Err(Diagnostic::new(
                        Code::DuplicateKwarg,
                        Span::new(base as usize + arg_start, name.len()),
                    ));
                }
                args.keyword.insert(name, v);
            } else {
                *cursor = saved;
                let v = read_value(src, cursor).ok_or_else(|| {
                    Diagnostic::new(Code::BadArgSyntax, char_span(src, *cursor, base))
                })?;
                args.positional.push(v);
            }
        } else {
            let v = read_value(src, cursor).ok_or_else(|| {
                Diagnostic::new(Code::BadArgSyntax, char_span(src, *cursor, base))
            })?;
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
            _ => {
                return Err(Diagnostic::new(
                    Code::BadArgSyntax,
                    char_span(src, *cursor, base),
                ));
            }
        }
    }
    Ok(args)
}

/// The character at `at` (empty at end of input), so a caret never
/// splits a multibyte character or runs past the line.
fn char_span(src: &str, at: usize, base: u32) -> Span {
    let len = src
        .get(at..)
        .and_then(|s| s.chars().next())
        .map_or(0, char::len_utf8);
    Span::new(base as usize + at, len)
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
        let args = parse_args(s, &mut cursor, 0).unwrap();
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

    fn codes(d: &[Diagnostic]) -> Vec<(Code, u32, u32)> {
        d.iter()
            .map(|x| (x.code, x.span.start, x.span.len))
            .collect()
    }

    fn texts(n: &[Inline]) -> Vec<&str> {
        n.iter()
            .map(|x| match x {
                Inline::Text { value, .. } => value.as_str(),
                _ => "<node>",
            })
            .collect()
    }

    #[test]
    fn alternating_emphasis_is_linear_not_a_stack_overflow() {
        // 48 KB of `*a _a ` used to recurse once per marker and abort the
        // process. Same-kind nesting is illegal, so the inner markers are
        // B0204s, the two openers B0207s, and no text is lost.
        let input = "*a _a ".repeat(8000);
        let (n, d) = parse(&input);
        assert_eq!(d.len(), 16000);
        let unterminated: Vec<u32> = d
            .iter()
            .filter(|x| x.code == Code::UnterminatedEmph)
            .map(|x| x.span.start)
            .collect();
        assert_eq!(unterminated, vec![3, 0]);
        assert!(
            d.iter()
                .all(|x| matches!(x.code, Code::UnterminatedEmph | Code::EmphasisSameMarker))
        );
        assert_eq!(texts(&n).concat(), input);
    }

    #[test]
    fn deep_shortcode_nesting_is_b0318_not_a_crash() {
        let input = format!("{}x{}", "@sup[".repeat(5000), "]".repeat(5000));
        let (_, d) = parse(&input);
        // The 65th body is refused; reported once.
        assert_eq!(codes(&d), vec![(Code::NestingTooDeep, 64 * 5, 1)]);
    }

    #[test]
    fn same_kind_nesting_through_another_kind_is_b0204() {
        let (n, d) = parse("*a _b *c* d_ e*");
        assert_eq!(codes(&d), vec![(Code::EmphasisSameMarker, 6, 1)]);
        let Inline::Bold { content, .. } = &n[0] else {
            panic!("{:?}", n);
        };
        let Inline::Italic { content: inner, .. } = &content[1] else {
            panic!("{:?}", content);
        };
        assert_eq!(texts(inner), vec!["b *c* d"]);
    }

    #[test]
    fn unclosed_emphasis_keeps_its_text() {
        let (n, d) = parse("*unclosed bold");
        assert_eq!(codes(&d), vec![(Code::UnterminatedEmph, 0, 1)]);
        assert_eq!(texts(&n), vec!["*", "unclosed bold"]);

        // Children parsed inside the unclosed span survive as siblings.
        let (n, d) = parse("a *b _c_ d");
        assert_eq!(codes(&d), vec![(Code::UnterminatedEmph, 2, 1)]);
        assert_eq!(texts(&n), vec!["a ", "*", "b ", "<node>", " d"]);
        assert!(matches!(n[3], Inline::Italic { .. }));
    }

    #[test]
    fn unclosed_emphasis_stops_at_the_shortcode_bracket() {
        let (n, d) = parse("@kbd[*x] y");
        assert_eq!(codes(&d), vec![(Code::UnterminatedEmph, 5, 1)]);
        let Inline::Shortcode { content, .. } = &n[0] else {
            panic!("{:?}", n);
        };
        assert_eq!(texts(content.as_ref().unwrap()), vec!["*", "x"]);
        assert_eq!(texts(&n[1..]), vec![" y"]);
    }

    #[test]
    fn unclosed_shortcode_bracket_is_b0406() {
        let (_, d) = parse("@kbd[Ctrl");
        assert_eq!(codes(&d), vec![(Code::BadArgSyntax, 4, 1)]);
    }

    #[test]
    fn math_body_is_verbatim() {
        let (n, d) = parse(r"@math[\frac{a}{b} < c *x* `y` \_]");
        assert!(d.is_empty(), "{:?}", d);
        let Inline::Shortcode { content, span, .. } = &n[0] else {
            panic!("{:?}", n);
        };
        assert_eq!(
            texts(content.as_ref().unwrap()),
            vec![r"\frac{a}{b} < c *x* `y` \_"]
        );
        assert_eq!((span.start, span.len), (0, 33));
    }

    #[test]
    fn math_body_ends_at_the_balancing_bracket() {
        // The rule: brackets inside `@math[...]` nest, and the body ends at
        // the `]` that balances the opening `[`.
        let (n, d) = parse(r"@math[\sqrt[3]{x} + [a, b]] after");
        assert!(d.is_empty(), "{:?}", d);
        let Inline::Shortcode { content, .. } = &n[0] else {
            panic!("{:?}", n);
        };
        assert_eq!(
            texts(content.as_ref().unwrap()),
            vec![r"\sqrt[3]{x} + [a, b]"]
        );
        assert_eq!(texts(&n[1..]), vec![" after"]);

        // An unmatched bracket leaves the body open: an error, never a
        // silently truncated formula.
        let (_, d) = parse(r"@math[x \in [0, 1)] y");
        assert_eq!(codes(&d), vec![(Code::BadArgSyntax, 5, 1)]);
        let (n, d) = parse(r"@math[x \in \lbrack 0, 1)]");
        assert!(d.is_empty(), "{:?}", d);
        let Inline::Shortcode { content, .. } = &n[0] else {
            panic!("{:?}", n);
        };
        assert_eq!(
            texts(content.as_ref().unwrap()),
            vec![r"x \in \lbrack 0, 1)"]
        );
    }

    #[test]
    fn comment_after_content_is_b0307() {
        let (n, d) = parse("text /* note */ more");
        assert_eq!(codes(&d), vec![(Code::InlineBlockComment, 5, 10)]);
        // Reported once: the `*` of `*/` must not also open emphasis.
        assert_eq!(texts(&n), vec!["text /* note */ more"]);
    }

    #[test]
    fn globs_paths_and_code_are_not_comments() {
        for s in [
            r"rm /\*.tmp",
            "a/*b and c*/d",
            "ls /* | wc",
            "`a /* b */ c`",
            r"x \/* y */",
            "@math[a /* b */ c]",
            "/*/",
        ] {
            let (_, d) = parse(s);
            assert!(
                !d.iter().any(|x| x.code == Code::InlineBlockComment),
                "{}: {:?}",
                s,
                d
            );
        }
    }

    #[test]
    fn arg_error_spans_cover_whole_chars() {
        // A one-byte span on `é` ends mid-character and used to crash the
        // LSP when it sliced the source.
        let mut cursor = 0usize;
        let err = parse_args("(kind: é)", &mut cursor, 100).unwrap_err();
        assert_eq!(
            (err.code, err.span.start, err.span.len),
            (Code::BadArgSyntax, 107, 2)
        );
        // At end of input the span is empty rather than one past the line.
        let mut cursor = 0usize;
        let err = parse_args("(a: \"x", &mut cursor, 10).unwrap_err();
        assert_eq!((err.span.start, err.span.len), (16, 0));
    }
}
