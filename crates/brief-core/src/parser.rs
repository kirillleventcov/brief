//! Builds the [`Document`] block tree from the lexer's line tokens.

use crate::ast::{
    Block, CodeAttrs, DefinitionItem, Document, Inline, ListItem, Row, ShortcodeArgs, TaskState,
};
use crate::diag::{Code, Diagnostic};
use crate::inline::{self, Seg};
use crate::shortcode::ArgValue;
use crate::span::{SourceMap, Span};
use crate::token::{Token, TokenKind};
use std::mem;
use std::ops::Range;
use toml::Table;

/// Parses the tokens [`crate::lexer::lex`] produced for `src` into a
/// document, alongside every diagnostic found. Parsing never stops at the
/// first error: malformed constructs are reported and skipped or degraded
/// to empty paragraphs.
///
/// # Panics
///
/// Panics if `tokens` does not end with a [`TokenKind::Eof`] token, which
/// `lex` always appends.
#[must_use]
pub fn parse(tokens: Vec<Token>, src: &SourceMap) -> (Document, Vec<Diagnostic>) {
    let Frontmatter {
        metadata,
        consumed,
        body,
        diags,
    } = parse_frontmatter(&tokens, src);
    let mut p = Parser {
        src,
        toks: tokens,
        pos: consumed,
        diags,
        depth: 0,
        verbatim: body.into_iter().collect(),
        seg_buf: Vec::new(),
    };
    let mut blocks = p.parse_blocks(0, None);
    // Anything left after a top-level parse must be a stray `@end`.
    while !p.at_eof() {
        let tok = *p.peek();
        match tok.kind {
            TokenKind::Eof => break,
            TokenKind::Blank => {
                p.pos += 1;
            }
            TokenKind::Line if p.line(&tok).trim() == "@end" => {
                p.diags.push(Diagnostic::new(Code::StrayEnd, tok.span));
                p.pos += 1;
            }
            _ => {
                blocks.append(&mut p.parse_blocks(0, None));
            }
        }
    }
    let mut diags = p.tab_diagnostics();
    diags.append(&mut p.diags);
    (
        Document {
            blocks,
            metadata,
            resolved_refs: Default::default(),
        },
        diags,
    )
}

/// Outcome of `parse_frontmatter`.
struct Frontmatter {
    metadata: Option<Table>,
    /// Tokens consumed, including both `+++` lines.
    consumed: usize,
    /// Token range of the TOML body, if the frontmatter was closed.
    body: Option<Range<usize>>,
    diags: Vec<Diagnostic>,
}

impl Frontmatter {
    fn none() -> Self {
        Frontmatter {
            metadata: None,
            consumed: 0,
            body: None,
            diags: Vec::new(),
        }
    }
}

fn parse_frontmatter(toks: &[Token], src: &SourceMap) -> Frontmatter {
    // Frontmatter must be the very first content. We detect it by checking
    // that the first token is `Line("+++")` at byte offset 0 with indent 0.
    // If anything else comes first (a Blank, a comment, an indented `+++`,
    // or a non-`+++` line), there is no frontmatter.
    if toks.is_empty() {
        return Frontmatter::none();
    }
    let first = &toks[0];
    let opens = match first.kind {
        TokenKind::Line => {
            first.indent == 0 && first.span.start == 0 && &src.source[first.span.range()] == "+++"
        }
        _ => false,
    };
    if !opens {
        return Frontmatter::none();
    }

    // Body starts at the first byte after `+++\n` (or `+++\r\n`).
    // The lexer's next token's `span.start` is exactly that byte.
    let mut idx = 1usize;
    let body_start = toks
        .get(idx)
        .map(|t| t.span.start as usize)
        .unwrap_or(src.source.len());

    let mut diags = Vec::new();
    while idx < toks.len() {
        match toks[idx].kind {
            TokenKind::Eof => {
                diags.push(
                    Diagnostic::new(Code::UnterminatedFrontmatter, first.span)
                        .label("frontmatter opened with `+++` is never closed"),
                );
                return Frontmatter {
                    metadata: None,
                    consumed: idx,
                    body: None,
                    diags,
                };
            }
            TokenKind::Line
                if toks[idx].indent == 0 && &src.source[toks[idx].span.range()] == "+++" =>
            {
                let close = &toks[idx];
                let body_end = close.span.start as usize;
                let body = &src.source[body_start..body_end];
                // TOML is opaque to Brief: its tabs are not Brief's to ban.
                let body_toks = Some(1..idx);
                idx += 1; // consume the closing `+++`
                match toml::from_str::<Table>(body) {
                    Ok(t) => {
                        return Frontmatter {
                            metadata: Some(t),
                            consumed: idx,
                            body: body_toks,
                            diags,
                        };
                    }
                    Err(e) => {
                        let (off, len) = match e.span() {
                            Some(r) => (body_start + r.start, r.end - r.start),
                            None => (body_start, body.len()),
                        };
                        // A zero-length span gets the width of the character
                        // it points at so the caret renders without
                        // splitting a multibyte character.
                        let len = if len == 0 {
                            src.source
                                .get(off..)
                                .and_then(|s| s.chars().next())
                                .map_or(0, char::len_utf8)
                        } else {
                            len
                        };
                        let span = Span::new(off, len);
                        diags.push(
                            Diagnostic::new(Code::FrontmatterToml, span).label(e.to_string()),
                        );
                        return Frontmatter {
                            metadata: None,
                            consumed: idx,
                            body: body_toks,
                            diags,
                        };
                    }
                }
            }
            _ => {
                idx += 1;
            }
        }
    }
    diags.push(
        Diagnostic::new(Code::UnterminatedFrontmatter, first.span)
            .label("frontmatter opened with `+++` is never closed"),
    );
    Frontmatter {
        metadata: None,
        consumed: idx,
        body: None,
        diags,
    }
}

/// Blocks may nest via list children, block shortcodes, and blockquote
/// markers. Each level recurses on the native stack, so nesting must be
/// bounded or hostile/degenerate input aborts the process with a stack
/// overflow instead of a diagnostic.
pub(crate) const MAX_NESTING_DEPTH: usize = 64;

struct Parser<'src> {
    src: &'src SourceMap,
    toks: Vec<Token>,
    pos: usize,
    diags: Vec<Diagnostic>,
    depth: usize,
    /// Token ranges whose lines reach the output byte-for-byte (code fence
    /// and `@code` bodies, frontmatter), in source order. Tabs are legal
    /// there and trailing spaces are kept.
    verbatim: Vec<Range<usize>>,
    /// Scratch for `parse_paragraph`'s segment table.
    seg_buf: Vec<Seg>,
}

impl<'src> Parser<'src> {
    fn peek(&self) -> &Token {
        &self.toks[self.pos]
    }
    /// Text of a `Line` token, sliced from the source. The returned slice
    /// borrows from the `SourceMap` (lifetime `'src`), not from `self`, so
    /// it stays valid across `&mut self` calls.
    fn line(&self, tok: &Token) -> &'src str {
        &self.src.source[tok.span.range()]
    }
    /// The whole source line of token `idx` with only its terminator (`\n`
    /// or `\r\n`) removed: trailing spaces and tabs intact. Verbatim bodies
    /// read this; everything else reads the lexer's trimmed `line`. The
    /// lexer emits one token per line, so the next token starts right
    /// after this line's newline.
    fn raw_line(&self, idx: usize) -> &'src str {
        let src = self.src.source.as_str();
        let start = self.toks[idx].span.start as usize;
        let next = self
            .toks
            .get(idx + 1)
            .map_or(src.len(), |t| t.span.start as usize);
        let line = &src[start..next];
        let line = line.strip_suffix('\n').unwrap_or(line);
        line.strip_suffix('\r').unwrap_or(line)
    }
    fn at_eof(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }

    /// B0102 for every tab outside a verbatim region. This runs after the
    /// parse because only the parser knows which lines are code: a tab in
    /// a Go or Makefile fence is content, not indentation. Tokens carry no
    /// tab flag (an extra field costs the lexer's push loop ~35%), so lines
    /// are rescanned, but only in a document that holds a tab at all.
    fn tab_diagnostics(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if !self.src.source.contains('\t') {
            return out;
        }
        let mut regions = self.verbatim.iter().peekable();
        for (i, tok) in self.toks.iter().enumerate() {
            if matches!(tok.kind, TokenKind::Eof) {
                continue;
            }
            let raw = self.raw_line(i);
            if !raw.as_bytes().contains(&b'\t') {
                continue;
            }
            while regions.next_if(|r| r.end <= i).is_some() {}
            if regions.peek().is_some_and(|r| r.contains(&i)) {
                continue;
            }
            let start = tok.span.start as usize;
            for (j, b) in raw.bytes().enumerate() {
                if b == b'\t' {
                    out.push(
                        Diagnostic::new(Code::TabCharacter, Span::new(start + j, 1))
                            .help("tabs are forbidden; use two spaces per indent level"),
                    );
                }
            }
        }
        out
    }

    fn parse_blocks(&mut self, base_indent: u16, end_at_indent_below: Option<u16>) -> Vec<Block> {
        if self.depth >= MAX_NESTING_DEPTH {
            self.diags.push(
                Diagnostic::new(Code::NestingTooDeep, self.peek().span).label(format!(
                    "blocks nest deeper than {} levels",
                    MAX_NESTING_DEPTH
                )),
            );
            self.skip_overdeep_region(base_indent, end_at_indent_below);
            return Vec::new();
        }
        self.depth += 1;
        let out = self.parse_blocks_inner(base_indent, end_at_indent_below);
        self.depth -= 1;
        out
    }

    /// Consume the tokens a `parse_blocks` call would have owned, without
    /// recursing, so callers past the depth ceiling still make progress.
    fn skip_overdeep_region(&mut self, base_indent: u16, end_at_indent_below: Option<u16>) {
        while !self.at_eof() {
            let tok = *self.peek();
            match tok.kind {
                TokenKind::Eof => break,
                TokenKind::Blank => {
                    self.pos += 1;
                }
                TokenKind::Line => {
                    let indent = tok.indent;
                    if indent < base_indent {
                        break;
                    }
                    if let Some(min) = end_at_indent_below
                        && indent < min
                    {
                        break;
                    }
                    if self.line(&tok)[indent as usize..].trim() == "@end" {
                        break;
                    }
                    self.pos += 1;
                }
            }
        }
    }

    fn parse_blocks_inner(
        &mut self,
        base_indent: u16,
        end_at_indent_below: Option<u16>,
    ) -> Vec<Block> {
        let mut out = Vec::new();
        loop {
            if self.at_eof() {
                break;
            }
            match &self.peek().kind {
                TokenKind::Eof => break,
                TokenKind::Blank => {
                    self.pos += 1;
                    continue;
                }
                TokenKind::Line => {
                    let tok = *self.peek();
                    let indent = tok.indent;
                    if indent < base_indent {
                        break;
                    }
                    if let Some(min) = end_at_indent_below
                        && indent < min
                    {
                        break;
                    }
                    let trimmed = &self.line(&tok)[indent as usize..];

                    // `@end` is always a terminator for the parent block-shortcode;
                    // stop here so the caller can consume it. Stray @ends are
                    // surfaced by the top-level driver in `parse()`.
                    if trimmed.trim() == "@end" {
                        break;
                    }
                    if trimmed.starts_with("//") {
                        self.pos += 1;
                        continue;
                    }
                    if opens_block_comment(trimmed) {
                        self.consume_block_comment(trimmed);
                        continue;
                    }
                    if let Some(b) = self.try_block_at(trimmed, indent) {
                        out.push(b);
                    } else {
                        out.push(self.parse_paragraph(indent));
                    }
                }
            }
        }
        out
    }

    fn try_block_at(&mut self, trimmed: &str, indent: u16) -> Option<Block> {
        if trimmed.starts_with('#') {
            return Some(self.parse_heading());
        }
        if trimmed == "---" {
            return Some(self.parse_hr());
        }
        // A dash-only line that isn't exactly three dashes is a malformed
        // horizontal rule, not prose — silently reading it as a paragraph
        // would hide the typo.
        if trimmed.len() >= 2 && trimmed.bytes().all(|b| b == b'-') {
            let span = self.peek().span;
            self.diags.push(
                Diagnostic::new(Code::BadHorizontalRule, span).label(format!(
                    "horizontal rule is exactly `---`, got {} dashes",
                    trimmed.len()
                )),
            );
            self.pos += 1;
            return Some(Block::Paragraph {
                content: vec![],
                span,
            });
        }
        if trimmed.starts_with("```") {
            return Some(self.parse_code_fence());
        }
        if trimmed.starts_with("- ") {
            return Some(self.parse_unordered_list(indent));
        }
        if leading_ordered_marker(trimmed).is_some() {
            return Some(self.parse_ordered_list(indent));
        }
        if trimmed.starts_with('>') {
            return Some(self.parse_blockquote(indent));
        }
        if trimmed == "@t" || trimmed.starts_with("@t ") || trimmed.starts_with("@t(") {
            return Some(self.parse_table(indent));
        }
        if trimmed == "@dl" || trimmed.starts_with("@dl ") || trimmed.starts_with("@dl(") {
            return Some(self.parse_definition_list(indent));
        }
        if trimmed.starts_with('@') {
            return self.parse_block_shortcode_or_inline(indent);
        }
        if trimmed.starts_with('|') {
            let span = self.peek().span;
            self.diags.push(
                Diagnostic::new(Code::StrayContent, span)
                    .label("`|` only appears inside a `@t` table"),
            );
            self.pos += 1;
            return Some(Block::Paragraph {
                content: vec![],
                span,
            });
        }
        None
    }

    fn consume_block_comment(&mut self, trimmed: &str) {
        if trimmed.ends_with("*/") && trimmed.len() >= 4 {
            self.pos += 1;
            return;
        }
        let open = *self.peek();
        self.pos += 1;
        loop {
            let tok = *self.peek();
            match tok.kind {
                TokenKind::Eof => {
                    // Point at the opener: that is the line to fix, and the
                    // whole rest of the file hangs on it.
                    let at = Span::new(open.span.start as usize + open.indent as usize, 2);
                    self.diags.push(
                        Diagnostic::new(Code::UnterminatedComment, at)
                            .label("comment opened here is never closed"),
                    );
                    return;
                }
                TokenKind::Blank => {
                    self.pos += 1;
                }
                TokenKind::Line => {
                    let s = self.line(&tok);
                    self.pos += 1;
                    if s.trim_end().ends_with("*/") {
                        return;
                    }
                }
            }
        }
    }

    fn parse_heading(&mut self) -> Block {
        let tok = *self.peek();
        let line = self.line(&tok);
        self.pos += 1;
        let indent = tok.indent as usize;
        let s = &line[indent..];
        let mut level = 0u8;
        let bytes = s.as_bytes();
        while (level as usize) < bytes.len() && bytes[level as usize] == b'#' {
            level += 1;
            if level > 6 {
                break;
            }
        }
        let mut hash_count = level as usize;
        while hash_count < bytes.len() && bytes[hash_count] == b'#' {
            hash_count += 1;
        }
        if hash_count > 6 {
            let span = Span::new(tok.span.start as usize + indent, hash_count);
            self.diags.push(
                Diagnostic::new(Code::HeadingTooDeep, span)
                    .label("Brief supports heading levels 1-6 only"),
            );
            return Block::Paragraph {
                content: vec![],
                span: tok.span,
            };
        }
        if bytes.get(level as usize) != Some(&b' ') {
            self.diags.push(
                Diagnostic::new(Code::HeadingNoSpace, tok.span)
                    .help("write `# heading` with exactly one space after the `#`s"),
            );
            return Block::Paragraph {
                content: vec![],
                span: tok.span,
            };
        }
        if bytes.get(level as usize + 1) == Some(&b' ') {
            self.diags.push(
                Diagnostic::new(Code::HeadingNoSpace, tok.span)
                    .label("multiple spaces after heading marker"),
            );
        }
        let text_offset = indent + level as usize + 1;
        let raw_text = &line[text_offset..];

        // Anchor detection: look for a trailing `{#name}` block.
        // Only triggers when the line ends with `}` and contains `{#`.
        let (heading_text, anchor) = parse_heading_anchor(
            raw_text,
            tok.span.start + text_offset as u32,
            &mut self.diags,
        );

        let (content, idiags) =
            inline::parse_inline(heading_text, tok.span.start + text_offset as u32);
        self.diags.extend(idiags);
        Block::Heading {
            level,
            content,
            anchor,
            span: tok.span,
        }
    }

    fn parse_paragraph(&mut self, indent: u16) -> Block {
        let first = *self.peek();
        let mut span = first.span;
        let mut text = String::new();
        // Fast path: most paragraphs are a single line with no hard break.
        // Hold the first line as a source slice and only spill into the
        // owned `text` accumulator when a second line (or a hard break)
        // makes joining unavoidable.
        let mut single: Option<&'src str> = None;
        // (end of the line's text in `text`, source offset of its `\`).
        let mut hard_breaks: Vec<(usize, usize)> = Vec::new();
        // Where each joined line starts in `text` and in the source. The
        // buffer is reused across paragraphs to keep allocation off the
        // common path.
        let mut segs = mem::take(&mut self.seg_buf);
        segs.clear();
        // Source offset just past the previous line's hard-break `\`.
        let mut after_backslash: Option<u32> = None;
        let mut first_line = true;
        loop {
            let tok = *self.peek();
            match tok.kind {
                TokenKind::Line => {
                    if tok.indent != indent {
                        break;
                    }
                    let trimmed = &self.line(&tok)[indent as usize..];
                    // The first paragraph line is always consumed: the dispatcher
                    // already decided this isn't a block. Subsequent continuation
                    // lines stop at any block-starting sigil.
                    if !first_line && leading_block_sigil(trimmed) {
                        break;
                    }
                    let (line_text, hard) = match trimmed.strip_suffix('\\') {
                        Some(rest) => (rest, true),
                        None => (trimmed, false),
                    };
                    let source = tok.span.start + indent as u32;
                    if first_line && !hard {
                        single = Some(line_text);
                    } else {
                        if let Some(prev) = single.take() {
                            segs.push(Seg {
                                joined: 0,
                                source: first.span.start + indent as u32,
                            });
                            text.push_str(prev);
                        }
                        if !text.is_empty() {
                            // The joining space stands for the line break.
                            // After a plain line the previous segment already
                            // maps it there; after a hard break the `\` sits
                            // in between, so it needs a segment of its own.
                            if let Some(source) = after_backslash {
                                segs.push(Seg {
                                    joined: text.len() as u32,
                                    source,
                                });
                            }
                            text.push(' ');
                        }
                        segs.push(Seg {
                            joined: text.len() as u32,
                            source,
                        });
                        if hard {
                            hard_breaks.push((text.len() + line_text.len(), tok.span.end() - 1));
                        }
                        text.push_str(line_text);
                    }
                    first_line = false;
                    after_backslash = hard.then(|| tok.span.end() as u32);
                    span = span.join(tok.span);
                    self.pos += 1;
                }
                _ => break,
            }
        }
        if let Some(s) = single {
            self.seg_buf = segs;
            let base = first.span.start + first.indent as u32;
            let (content, d) = inline::parse_inline(s, base);
            self.diags.extend(d);
            return Block::Paragraph { content, span };
        }
        let mut content: Vec<Inline> = Vec::new();
        let mut cursor = 0usize;
        for &(hb, backslash) in &hard_breaks {
            let chunk = &text[cursor..hb];
            let (mut inl, d) = inline::parse_inline_joined(chunk, cursor, &segs);
            self.diags.extend(d);
            content.append(&mut inl);
            content.push(Inline::HardBreak {
                span: Span::new(backslash, 1),
            });
            cursor = hb;
        }
        let chunk = &text[cursor..];
        let (mut inl, d) = inline::parse_inline_joined(chunk, cursor, &segs);
        self.diags.extend(d);
        content.append(&mut inl);
        self.seg_buf = segs;
        Block::Paragraph { content, span }
    }

    fn parse_hr(&mut self) -> Block {
        let tok = *self.peek();
        self.pos += 1;
        Block::HorizontalRule { span: tok.span }
    }

    fn parse_code_fence(&mut self) -> Block {
        let open = *self.peek();
        let line = self.line(&open);
        self.pos += 1;
        let indent = open.indent as usize;
        let after = &line[indent + 3..];
        if after.starts_with('`') {
            self.diags.push(
                Diagnostic::new(Code::UnterminatedFence, open.span)
                    .label("opening fence must be exactly three backticks"),
            );
        }
        let info_offset = open.span.start as usize + indent + 3;
        let (lang, attrs) = parse_fence_info(after, info_offset as u32, open.span, &mut self.diags);
        let body_start = self.pos;
        let mut body = String::new();
        let mut span = open.span;
        loop {
            let tok = *self.peek();
            match tok.kind {
                TokenKind::Eof => {
                    self.diags.push(
                        Diagnostic::new(Code::UnterminatedFence, open.span)
                            .label("fence opened here is never closed"),
                    );
                    self.verbatim.push(body_start..self.pos);
                    break;
                }
                TokenKind::Line if self.line(&tok).trim() == "```" => {
                    span = span.join(tok.span);
                    self.verbatim.push(body_start..self.pos);
                    self.pos += 1;
                    break;
                }
                TokenKind::Line | TokenKind::Blank => {
                    body.push_str(self.raw_line(self.pos));
                    body.push('\n');
                    span = span.join(tok.span);
                    self.pos += 1;
                }
            }
        }
        if body.ends_with('\n') {
            body.pop();
        }
        Block::CodeBlock {
            lang,
            body,
            attrs,
            span,
        }
    }

    fn parse_unordered_list(&mut self, indent: u16) -> Block {
        let start_span = self.peek().span;
        let mut items: Vec<ListItem> = Vec::new();
        loop {
            if self.at_eof() {
                break;
            }
            let tok = *self.peek();
            if !matches!(tok.kind, TokenKind::Line) {
                break;
            }
            if tok.indent != indent {
                break;
            }
            let trimmed = &self.line(&tok)[indent as usize..];
            if !trimmed.starts_with("- ") {
                break;
            }
            let after_marker = &trimmed[2..];
            // Task-list modifier: exactly `[x] ` (Done) or `[ ] ` (Todo) at
            // the start of item content. Lowercase `x` only; one space; one
            // marker length only. Anything else is plain inline content.
            let (task, item_text, content_offset) =
                if let Some(rest) = after_marker.strip_prefix("[x] ") {
                    (Some(TaskState::Done), rest, 4u32)
                } else if let Some(rest) = after_marker.strip_prefix("[ ] ") {
                    (Some(TaskState::Todo), rest, 4u32)
                } else {
                    (None, after_marker, 0u32)
                };
            let (content, d) = inline::parse_inline(
                item_text,
                tok.span.start + indent as u32 + 2 + content_offset,
            );
            self.diags.extend(d);
            self.pos += 1;
            let mut children: Vec<Block> = Vec::new();
            self.skip_blanks();
            if matches!(self.peek().kind, TokenKind::Line) && self.peek().indent >= indent + 2 {
                children = self.parse_blocks(indent + 2, Some(indent + 2));
            }
            items.push(ListItem {
                content,
                children,
                task,
                span: tok.span,
            });
        }
        let span = items.iter().fold(start_span, |a, it| a.join(it.span));
        Block::List {
            ordered: false,
            items,
            span,
        }
    }

    fn parse_ordered_list(&mut self, indent: u16) -> Block {
        let start_span = self.peek().span;
        let mut items: Vec<ListItem> = Vec::new();
        let mut expected: u32 = 1;
        loop {
            if self.at_eof() {
                break;
            }
            let tok = *self.peek();
            if !matches!(tok.kind, TokenKind::Line) {
                break;
            }
            if tok.indent != indent {
                break;
            }
            let trimmed = &self.line(&tok)[indent as usize..];
            let Some((num, marker_len)) = leading_ordered_marker(trimmed) else {
                break;
            };
            if num != expected {
                let span = Span::new(tok.span.start as usize + indent as usize, marker_len);
                self.diags.push(
                    Diagnostic::new(Code::OrderedListSequence, span)
                        .label(format!("got `{}.`, expected `{}.`", num, expected))
                        .help("ordered lists must number sequentially starting from 1"),
                );
            }
            expected = expected.saturating_add(1);
            let after_marker = &trimmed[marker_len..];
            // Task-list modifier: exactly `[x] ` (Done) or `[ ] ` (Todo) at
            // the start of item content. Lowercase `x` only; one space; one
            // marker length only. Anything else is plain inline content.
            let (task, item_text, content_offset) =
                if let Some(rest) = after_marker.strip_prefix("[x] ") {
                    (Some(TaskState::Done), rest, 4u32)
                } else if let Some(rest) = after_marker.strip_prefix("[ ] ") {
                    (Some(TaskState::Todo), rest, 4u32)
                } else {
                    (None, after_marker, 0u32)
                };
            let (content, d) = inline::parse_inline(
                item_text,
                tok.span.start + indent as u32 + marker_len as u32 + content_offset,
            );
            self.diags.extend(d);
            self.pos += 1;
            let mut children: Vec<Block> = Vec::new();
            self.skip_blanks();
            if matches!(self.peek().kind, TokenKind::Line) && self.peek().indent >= indent + 2 {
                children = self.parse_blocks(indent + 2, Some(indent + 2));
            }
            items.push(ListItem {
                content,
                children,
                task,
                span: tok.span,
            });
        }
        let span = items.iter().fold(start_span, |a, it| a.join(it.span));
        Block::List {
            ordered: true,
            items,
            span,
        }
    }

    fn parse_blockquote(&mut self, indent: u16) -> Block {
        let mut lines: Vec<QuoteLine<'src>> = Vec::new();
        let start = self.peek().span;
        loop {
            if self.at_eof() {
                break;
            }
            let tok = *self.peek();
            if !matches!(tok.kind, TokenKind::Line) {
                break;
            }
            if tok.indent != indent {
                break;
            }
            let trimmed = &self.line(&tok)[indent as usize..];
            let mut depth: usize = 0;
            let mut idx = 0usize;
            let bytes = trimmed.as_bytes();
            while idx < bytes.len() && bytes[idx] == b'>' {
                depth += 1;
                idx += 1;
            }
            if depth == 0 {
                break;
            }
            if depth > MAX_NESTING_DEPTH {
                self.diags.push(
                    Diagnostic::new(Code::NestingTooDeep, tok.span).label(format!(
                        "blockquote nests deeper than {} levels",
                        MAX_NESTING_DEPTH
                    )),
                );
                self.pos += 1;
                break;
            }
            if bytes.get(idx) != Some(&b' ') {
                self.diags.push(
                    Diagnostic::new(Code::BadBlockquote, tok.span)
                        .label("expected one space after `>`"),
                );
                self.pos += 1;
                break;
            }
            lines.push(QuoteLine {
                depth,
                body: &trimmed[idx + 1..],
                base: tok.span.start + indent as u32 + idx as u32 + 1,
                span: tok.span,
            });
            self.pos += 1;
        }
        let (children, span) = build_blockquote(&lines, 1, &mut self.diags);
        Block::Blockquote {
            children,
            span: if span == Span::DUMMY { start } else { span },
        }
    }

    fn parse_table(&mut self, indent: u16) -> Block {
        let directive = *self.peek();
        let line = self.line(&directive);
        self.pos += 1;
        let trimmed = &line[indent as usize..];
        let mut cursor = 2usize;
        let args = if trimmed.as_bytes().get(cursor) == Some(&b'(') {
            match inline::parse_args(trimmed, &mut cursor, directive.span.start + indent as u32) {
                Ok(a) => a,
                Err(d) => {
                    self.diags.push(d);
                    ShortcodeArgs::default()
                }
            }
        } else {
            ShortcodeArgs::default()
        };
        let mut rows: Vec<Row> = Vec::new();
        loop {
            if self.at_eof() {
                break;
            }
            let tok = *self.peek();
            if !matches!(tok.kind, TokenKind::Line) {
                break;
            }
            let row_line = self.line(&tok);
            let trimmed = row_line.trim_start();
            if !trimmed.starts_with('|') {
                break;
            }
            let split = split_cells(trimmed);
            if let Some(rel) = split.unclosed_backtick_at {
                // `rel` is relative to `trimmed`. The token spans `row_line`, which
                // includes leading whitespace; account for that when computing the
                // absolute offset into the SourceMap.
                let leading_ws = row_line.len() - trimmed.len();
                debug_assert!(
                    rel < trimmed.len(),
                    "unclosed_backtick_at {} out of bounds for trimmed (len {})",
                    rel,
                    trimmed.len()
                );
                let abs = tok.span.start as usize + leading_ws + rel;
                self.diags.push(
                    Diagnostic::new(Code::UnterminatedCode, Span::new(abs, 1))
                        .label("inline code span never closed inside a table row"),
                );
                self.pos += 1;
                continue;
            }
            let mut parsed_cells: Vec<Vec<Inline>> = Vec::new();
            for c in split.cells {
                let base = tok.span.start as usize + subslice_offset(row_line, c);
                let (inl, d) = inline::parse_inline(c, base as u32);
                self.diags.extend(d);
                parsed_cells.push(inl);
            }
            rows.push(Row {
                cells: parsed_cells,
                span: tok.span,
            });
            self.pos += 1;
        }
        if rows.is_empty() {
            self.diags.push(
                Diagnostic::new(Code::StrayContent, directive.span)
                    .label("`@t` must be followed by at least a header row"),
            );
            return Block::Paragraph {
                content: vec![],
                span: directive.span,
            };
        }
        let header = rows.remove(0);
        let cols = header.cells.len();
        for r in &rows {
            if r.cells.len() != cols {
                self.diags.push(
                    Diagnostic::new(Code::TableColumnMismatch, r.span).label(format!(
                        "table row has {} cells, expected {}",
                        r.cells.len(),
                        cols
                    )),
                );
            }
        }
        if let Some(ArgValue::Array(a)) = args.keyword.get("align") {
            if a.len() != cols {
                self.diags.push(
                    Diagnostic::new(Code::AlignArrayLength, directive.span).label(format!(
                        "`align` has {} entries but table has {} columns",
                        a.len(),
                        cols
                    )),
                );
            }
            // `@t` becomes a `Block::Table` before the resolver sees it, so
            // the enum check must live here. The emitter writes these values
            // into a style attribute; anything outside the allowed set is
            // both a user error and an injection vector.
            for v in a {
                let ok = matches!(v.as_str(), Some("left") | Some("right") | Some("center"));
                if !ok {
                    self.diags
                        .push(
                            Diagnostic::new(Code::BadEnumValue, directive.span).label(format!(
                                "`align` entries must be `left`, `right`, or `center`, got `{}`",
                                v.as_str().unwrap_or(v.type_name())
                            )),
                        );
                }
            }
        }
        let span = rows
            .iter()
            .fold(directive.span.join(header.span), |a, r| a.join(r.span));
        Block::Table {
            args,
            header,
            rows,
            span,
        }
    }

    fn parse_definition_list(&mut self, indent: u16) -> Block {
        let directive = *self.peek();
        let line = self.line(&directive);
        self.pos += 1;
        let trimmed = &line[indent as usize..];
        let mut cursor = 3usize;
        let args = if trimmed.as_bytes().get(cursor) == Some(&b'(') {
            match inline::parse_args(trimmed, &mut cursor, directive.span.start + indent as u32) {
                Ok(a) => a,
                Err(d) => {
                    self.diags.push(d);
                    ShortcodeArgs::default()
                }
            }
        } else {
            ShortcodeArgs::default()
        };

        let mut items: Vec<DefinitionItem> = Vec::new();
        let mut pending_term: Option<(Vec<Inline>, Span)> = None;
        let mut pending_def: Option<PendingDef> = None;
        let cont_indent = indent + 2;
        let mut end_span = directive.span;

        // Closes any open definition into items, paired with the pending term.
        let finalize_def = |items: &mut Vec<DefinitionItem>,
                            pending_term: &mut Option<(Vec<Inline>, Span)>,
                            pending_def: &mut Option<PendingDef>,
                            diags: &mut Vec<Diagnostic>| {
            if let Some(PendingDef { text, segs, span }) = pending_def.take() {
                let term_pair = pending_term.take();
                let (def_inl, dd) = inline::parse_inline_joined(&text, 0, &segs);
                diags.extend(dd);
                if let Some((term, t_span)) = term_pair {
                    let pair_span = t_span.join(span);
                    items.push(DefinitionItem {
                        term,
                        definition: def_inl,
                        span: pair_span,
                    });
                } else {
                    // Should not happen — a definition without a term is
                    // caught at the `: ` line itself.
                }
            }
        };

        loop {
            if self.at_eof() {
                self.diags.push(
                    Diagnostic::new(Code::UnterminatedBlock, directive.span)
                        .label("`@dl` block was never closed with `@end`"),
                );
                break;
            }
            let tok = *self.peek();
            match tok.kind {
                TokenKind::Eof => {
                    self.diags.push(
                        Diagnostic::new(Code::UnterminatedBlock, directive.span)
                            .label("`@dl` block was never closed with `@end`"),
                    );
                    break;
                }
                TokenKind::Blank => {
                    finalize_def(
                        &mut items,
                        &mut pending_term,
                        &mut pending_def,
                        &mut self.diags,
                    );
                    self.pos += 1;
                    continue;
                }
                TokenKind::Line => {
                    let s = self.line(&tok);
                    if let Some(pd) = pending_def.as_mut()
                        && tok.indent == cont_indent
                    {
                        // Continuation of the active definition.
                        let body = &s[cont_indent as usize..];
                        pd.text.push(' ');
                        pd.segs.push(Seg {
                            joined: pd.text.len() as u32,
                            source: tok.span.start + cont_indent as u32,
                        });
                        pd.text.push_str(body);
                        pd.span = pd.span.join(tok.span);
                        self.pos += 1;
                        continue;
                    }
                    if tok.indent < indent {
                        // Dedented past the directive: the `@end` is
                        // missing. Leave the line to the enclosing block.
                        self.diags.push(
                            Diagnostic::new(Code::UnterminatedBlock, directive.span)
                                .label("`@dl` block was never closed with `@end`"),
                        );
                        break;
                    }
                    if tok.indent != indent {
                        let label = if tok.indent == cont_indent {
                            "continuation line without a definition to continue"
                        } else {
                            "line is at neither the `@dl` indent nor a definition's continuation indent"
                        };
                        self.diags
                            .push(Diagnostic::new(Code::BadDefinitionList, tok.span).label(label));
                        self.pos += 1;
                        continue;
                    }
                    let body = &s[indent as usize..];
                    if body.trim() == "@end" {
                        finalize_def(
                            &mut items,
                            &mut pending_term,
                            &mut pending_def,
                            &mut self.diags,
                        );
                        end_span = tok.span;
                        self.pos += 1;
                        break;
                    }
                    if let Some(rest) = body.strip_prefix(": ") {
                        // The marker is exactly `: ` — colon, one space.
                        // Extra spaces would silently become part of the
                        // definition text, so they're rejected as documented.
                        if rest.starts_with(' ') {
                            self.diags.push(
                                Diagnostic::new(Code::BadDefinitionList, tok.span)
                                    .label("`:` must be followed by exactly one space"),
                            );
                            self.pos += 1;
                            continue;
                        }
                        if pending_term.is_none() && pending_def.is_none() {
                            self.diags.push(
                                Diagnostic::new(Code::BadDefinitionList, tok.span)
                                    .label("definition without a term"),
                            );
                            self.pos += 1;
                            continue;
                        }
                        if pending_def.is_some() {
                            self.diags.push(
                                Diagnostic::new(Code::BadDefinitionList, tok.span).label(
                                    "multiple definitions per term are not supported in v0.4",
                                ),
                            );
                            // Drop the duplicate definition: do not consume
                            // pending_term, do not start a new pending_def.
                            self.pos += 1;
                            continue;
                        }
                        let source = tok.span.start + indent as u32 + 2;
                        pending_def = Some(PendingDef {
                            text: rest.to_string(),
                            segs: vec![Seg { joined: 0, source }],
                            span: tok.span,
                        });
                        self.pos += 1;
                    } else if body.starts_with(':') {
                        self.diags.push(
                            Diagnostic::new(Code::BadDefinitionList, tok.span)
                                .label("`:` must be followed by exactly one space"),
                        );
                        self.pos += 1;
                    } else {
                        // Term line.
                        finalize_def(
                            &mut items,
                            &mut pending_term,
                            &mut pending_def,
                            &mut self.diags,
                        );
                        if let Some((_t, t_span)) = pending_term.take() {
                            self.diags.push(
                                Diagnostic::new(Code::BadDefinitionList, t_span)
                                    .label("term without a definition"),
                            );
                        }
                        let base = tok.span.start + indent as u32;
                        let (term, td) = inline::parse_inline(body, base);
                        self.diags.extend(td);
                        pending_term = Some((term, tok.span));
                        self.pos += 1;
                    }
                }
            }
        }

        // Final flush after EOF or @end.
        finalize_def(
            &mut items,
            &mut pending_term,
            &mut pending_def,
            &mut self.diags,
        );
        if let Some((_t, t_span)) = pending_term {
            self.diags.push(
                Diagnostic::new(Code::BadDefinitionList, t_span).label("term without a definition"),
            );
        }
        if items.is_empty() && !self.diags.iter().any(|d| d.code == Code::BadDefinitionList) {
            self.diags.push(
                Diagnostic::new(Code::BadDefinitionList, directive.span)
                    .label("`@dl` must contain at least one term/definition pair"),
            );
        }

        Block::DefinitionList {
            args,
            items,
            span: directive.span.join(end_span),
        }
    }

    fn parse_block_shortcode_or_inline(&mut self, indent: u16) -> Option<Block> {
        let tok = *self.peek();
        if !matches!(tok.kind, TokenKind::Line) {
            return None;
        }
        let trimmed = &self.line(&tok)[indent as usize..];
        let mut cursor = 1usize;
        let bytes = trimmed.as_bytes();
        if cursor >= bytes.len() || !bytes[cursor].is_ascii_alphabetic() {
            return None;
        }
        let name_start = cursor;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'-')
        {
            cursor += 1;
        }
        let name = trimmed[name_start..cursor].to_string();
        let mut args = ShortcodeArgs::default();
        let mut arg_err = None;
        if bytes.get(cursor) == Some(&b'(') {
            match inline::parse_args(trimmed, &mut cursor, tok.span.start + indent as u32) {
                Ok(a) => args = a,
                Err(d) => {
                    // Resume after the line's last `)` so a directive with a
                    // bad argument still pairs with its `@end` instead of
                    // degrading into a paragraph and a stray `@end`.
                    cursor = trimmed.rfind(')').map_or(trimmed.len(), |i| i + 1);
                    arg_err = Some(d);
                }
            }
        }
        if !trimmed[cursor..].trim().is_empty() {
            // Inline content: the paragraph parse reports any argument
            // error, so it isn't pushed twice.
            return None;
        }
        self.diags.extend(arg_err);
        self.pos += 1;
        if name == "code" {
            return Some(self.parse_code_shortcode_body(indent, &tok, args));
        }
        if name == "math" {
            return Some(self.parse_math_block_body(indent, &tok, args));
        }
        let children = self.parse_blocks(indent, Some(indent));
        let mut end_span = tok.span;
        let end_tok = *self.peek();
        match end_tok.kind {
            TokenKind::Line if end_tok.indent == indent && self.line(&end_tok).trim() == "@end" => {
                end_span = end_tok.span;
                self.pos += 1;
            }
            _ => {
                self.diags.push(
                    Diagnostic::new(Code::UnterminatedBlock, tok.span)
                        .label(format!("`@{}` block was never closed with `@end`", name)),
                );
            }
        }
        Some(Block::BlockShortcode {
            name,
            args,
            children,
            span: tok.span.join(end_span),
        })
    }

    /// `@code` bodies are verbatim, exactly like a triple-backtick fence —
    /// the shortcode exists so fence syntax itself can appear inside a code
    /// block. The body must never go through `parse_blocks`/`parse_inline`;
    /// that would reject ordinary code (`*ptr` reads as emphasis) and
    /// double-escape the rest.
    fn parse_code_shortcode_body(
        &mut self,
        indent: u16,
        open: &Token,
        args: ShortcodeArgs,
    ) -> Block {
        let lang = self.code_shortcode_lang(args, open.span);
        let body_start = self.pos;
        let mut body = String::new();
        let mut span = open.span;
        loop {
            let tok = *self.peek();
            match tok.kind {
                TokenKind::Eof => {
                    self.diags.push(
                        Diagnostic::new(Code::UnterminatedBlock, open.span)
                            .label("`@code` block was never closed with `@end`"),
                    );
                    self.verbatim.push(body_start..self.pos);
                    break;
                }
                TokenKind::Line if tok.indent == indent && self.line(&tok).trim() == "@end" => {
                    span = span.join(tok.span);
                    self.verbatim.push(body_start..self.pos);
                    self.pos += 1;
                    break;
                }
                TokenKind::Line | TokenKind::Blank => {
                    body.push_str(self.raw_line(self.pos));
                    body.push('\n');
                    span = span.join(tok.span);
                    self.pos += 1;
                }
            }
        }
        if body.ends_with('\n') {
            body.pop();
        }
        Block::CodeBlock {
            lang,
            body,
            attrs: CodeAttrs::default(),
            span,
        }
    }

    /// `@math` bodies are verbatim TeX, like `@code`: Brief's escapes and
    /// markup would eat TeX's backslashes and `_`/`*`. The body becomes a
    /// single text paragraph so emitters can recover the source exactly.
    /// Trailing whitespace is dropped as on any Brief line; TeX ignores it.
    fn parse_math_block_body(&mut self, indent: u16, open: &Token, args: ShortcodeArgs) -> Block {
        let mut body = String::new();
        let mut body_span: Option<Span> = None;
        let mut end_span = open.span;
        loop {
            let tok = *self.peek();
            match tok.kind {
                TokenKind::Eof => {
                    self.diags.push(
                        Diagnostic::new(Code::UnterminatedBlock, open.span)
                            .label("`@math` block was never closed with `@end`"),
                    );
                    break;
                }
                TokenKind::Line if tok.indent == indent && self.line(&tok).trim() == "@end" => {
                    end_span = tok.span;
                    self.pos += 1;
                    break;
                }
                TokenKind::Line | TokenKind::Blank => {
                    body.push_str(self.line(&tok));
                    body.push('\n');
                    body_span = Some(body_span.map_or(tok.span, |s| s.join(tok.span)));
                    self.pos += 1;
                }
            }
        }
        let body = body.trim_end_matches('\n');
        let children = match body_span {
            Some(span) if !body.is_empty() => vec![Block::Paragraph {
                content: vec![Inline::Text {
                    value: body.to_string(),
                    span,
                }],
                span,
            }],
            _ => Vec::new(),
        };
        Block::BlockShortcode {
            name: "math".to_string(),
            args,
            children,
            span: open.span.join(end_span),
        }
    }

    /// `@code` becomes a `CodeBlock` before the resolver ever sees it, so its
    /// arguments must be checked here: one optional `lang`, positional or
    /// keyword, string or ident.
    fn code_shortcode_lang(&mut self, args: ShortcodeArgs, span: Span) -> Option<String> {
        let mut lang: Option<String> = None;
        for (i, v) in args.positional.iter().enumerate() {
            if i > 0 {
                self.diags.push(
                    Diagnostic::new(Code::BadArgSyntax, span)
                        .label("`@code` takes at most one positional argument (the language)"),
                );
                break;
            }
            match v.as_str() {
                Some(s) => lang = Some(s.to_string()),
                None => {
                    self.diags
                        .push(Diagnostic::new(Code::ArgTypeMismatch, span).label(format!(
                            "argument `lang` has type {} but expected String",
                            v.type_name()
                        )));
                }
            }
        }
        for (kw, v) in &args.keyword {
            if kw != "lang" {
                self.diags.push(
                    Diagnostic::new(Code::BadArgSyntax, span)
                        .label(format!("`@code` has no argument named `{}`", kw)),
                );
                continue;
            }
            if lang.is_some() {
                self.diags.push(
                    Diagnostic::new(Code::DuplicateKwarg, span)
                        .label("`lang` given both positionally and by keyword"),
                );
                continue;
            }
            match v.as_str() {
                Some(s) => lang = Some(s.to_string()),
                None => {
                    self.diags
                        .push(Diagnostic::new(Code::ArgTypeMismatch, span).label(format!(
                            "argument `lang` has type {} but expected String",
                            v.type_name()
                        )));
                }
            }
        }
        lang
    }

    fn skip_blanks(&mut self) {
        while matches!(self.peek().kind, TokenKind::Blank) {
            self.pos += 1;
        }
    }
}

/// A definition being accumulated in an `@dl` block, before it is paired
/// with its term.
struct PendingDef {
    /// The definition's lines joined with single spaces.
    text: String,
    /// Where each joined line starts in `text` and in the source.
    segs: Vec<Seg>,
    /// Covers the definition's lines.
    span: Span,
}

/// One `>` line: its nesting depth, the text after the markers, and that
/// text's source offset.
struct QuoteLine<'src> {
    depth: usize,
    body: &'src str,
    base: u32,
    span: Span,
}

fn build_blockquote(
    items: &[QuoteLine],
    depth: usize,
    diags: &mut Vec<Diagnostic>,
) -> (Vec<Block>, Span) {
    let mut paras: Vec<Block> = Vec::new();
    let mut full_span = Span::DUMMY;
    let mut i = 0;
    while i < items.len() {
        let QuoteLine {
            depth: d,
            body,
            base,
            span,
        } = items[i];
        if d < depth {
            break;
        }
        full_span = if full_span == Span::DUMMY {
            span
        } else {
            full_span.join(span)
        };
        if d == depth {
            let (content, inline_diags) = inline::parse_inline(body, base);
            diags.extend(inline_diags);
            paras.push(Block::Paragraph { content, span });
            i += 1;
        } else {
            let mut j = i;
            while j < items.len() && items[j].depth > depth {
                j += 1;
            }
            let (children, child_span) = build_blockquote(&items[i..j], depth + 1, diags);
            paras.push(Block::Blockquote {
                children,
                span: child_span,
            });
            i = j;
        }
    }
    (paras, full_span)
}

fn parse_fence_info(
    after: &str,
    base: u32,
    fence_span: Span,
    diags: &mut Vec<Diagnostic>,
) -> (Option<String>, CodeAttrs) {
    // The info string follows the opening ```; the language tag is the first
    // whitespace-separated token, and any subsequent `@<ident>` tokens are
    // fence attributes (v0.2: `@nominify`, `@minify`).
    let mut attrs = CodeAttrs::default();
    let bytes = after.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    if i == bytes.len() {
        return (None, attrs);
    }
    let lang_start = i;
    while i < bytes.len() && bytes[i] != b' ' {
        i += 1;
    }
    let lang_tok = &after[lang_start..i];
    let lang = if lang_tok.is_empty() {
        None
    } else if lang_tok.starts_with('@') {
        // The first non-whitespace token is an attribute, not a language.
        i = lang_start;
        None
    } else {
        Some(lang_tok.to_string())
    };

    while i < bytes.len() {
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let tok_start = i;
        while i < bytes.len() && bytes[i] != b' ' {
            i += 1;
        }
        let tok = &after[tok_start..i];
        if tok.is_empty() {
            continue;
        }
        let tok_span = Span::new(base as usize + tok_start, tok.len());
        if !tok.starts_with('@') {
            diags.push(
                Diagnostic::new(Code::UnknownCodeAttribute, tok_span)
                    .label(format!("`{}` is not a valid code-fence attribute", tok))
                    .help("attributes must be `@`-prefixed identifiers (e.g. `@nominify`)"),
            );
            continue;
        }
        let name = &tok[1..];
        match name {
            "nominify" => {
                if attrs.minify || attrs.keep_comments {
                    diags.push(
                        Diagnostic::new(Code::ConflictingCodeAttributes, fence_span)
                            .label("`@nominify` conflicts with `@minify`/`@minify-keep-comments`"),
                    );
                }
                attrs.nominify = true;
            }
            "minify" => {
                if attrs.nominify {
                    diags.push(
                        Diagnostic::new(Code::ConflictingCodeAttributes, fence_span)
                            .label("`@nominify` and `@minify` cannot both be set"),
                    );
                }
                attrs.minify = true;
            }
            "minify-keep-comments" => {
                if attrs.nominify {
                    diags.push(
                        Diagnostic::new(Code::ConflictingCodeAttributes, fence_span)
                            .label("`@nominify` and `@minify-keep-comments` cannot both be set"),
                    );
                }
                attrs.keep_comments = true;
            }
            _ => {
                diags.push(
                    Diagnostic::new(Code::UnknownCodeAttribute, tok_span)
                        .label(format!("unknown code-fence attribute `{}`", tok))
                        .help("v0.4 supports `@nominify`, `@minify`, `@minify-keep-comments`"),
                );
            }
        }
    }
    (lang, attrs)
}

/// Parse a trailing `{#anchor}` block from heading text.
///
/// Returns `(text_to_use_for_inline_parse, anchor_name)`. If the anchor
/// block is present but malformed, a diagnostic is pushed and `anchor` is
/// `None`, but the malformed block is still stripped from `text_to_use`
/// where possible.
///
/// Anchor syntax is triggered whenever `{#` appears in the heading text.
/// If `{#` is present but the format does not match the strict form
/// (` {#name}` at end of line, name `[a-z0-9-]+`), it is a `BadHeadingAnchor`
/// error.
///
/// Exception: `{#` in the middle of text with NO closing `}` at
/// end-of-line is treated as plain text — it's clearly not intended as
/// anchor syntax.
///
/// `base` is the byte offset of `text` in the source, used for diagnostics.
fn parse_heading_anchor<'a>(
    text: &'a str,
    base: u32,
    diags: &mut Vec<Diagnostic>,
) -> (&'a str, Option<String>) {
    // Find the last `{#` occurrence (there can be at most one anchor block).
    // With no `{#` anywhere, no anchor syntax is possible.
    let Some(hash_open) = text.rfind("{#") else {
        return (text, None);
    };

    // Check if the `}` at end-of-line closes this `{#`.
    // Case A: `{#name}` at end of line (the only valid form).
    // Case B: `{#name}` NOT at end of line (content after `}`) → malformed.
    // Case C: no `}` after the `{#` at all → the `{#` is inside text, leave alone.

    let after_hash = &text[hash_open..];
    let Some(rbrace) = after_hash.find('}') else {
        // `{#` with no closing `}` anywhere → plain text, no error.
        return (text, None);
    };

    let candidate = &after_hash[..rbrace + 1]; // e.g. `{#abc}`
    let after_candidate = &after_hash[rbrace + 1..]; // what comes after `}`

    // If there is content after the `}`, the anchor block is NOT at end of line.
    // This is a BadHeadingAnchor (rule: no content after `}`).
    if !after_candidate.is_empty() {
        let anchor_span = Span::new(base as usize + hash_open, candidate.len());
        diags.push(
            Diagnostic::new(Code::BadHeadingAnchor, anchor_span)
                .label("anchor block must be `{#anchor}` with exactly one space before `{` and no content after `}`"),
        );
        // Leave the heading text alone (don't strip anything).
        return (text, None);
    }

    // The candidate `{#...}` IS at end of line.
    // Validate: exactly one space before `{`.
    let before = &text[..hash_open];

    let malformed = match before.strip_suffix(' ') {
        // Exactly one space before `{`, not two.
        Some(rest) => rest.ends_with(' '),
        // Nothing, or no space, before `{`.
        None => true,
    };

    // Extract the name (everything between `{#` and `}`).
    let name_part = &candidate[2..candidate.len() - 1]; // strip `{#` and `}`

    let anchor_span_start = base as usize + hash_open;
    let anchor_span = Span::new(anchor_span_start, candidate.len());

    if malformed {
        diags.push(
            Diagnostic::new(Code::BadHeadingAnchor, anchor_span)
                .label("anchor block must be `{#anchor}` with exactly one space before `{` and no content after `}`"),
        );
        // Strip the malformed block from text.
        return (&text[..hash_open], None);
    }

    // Validate the name: `[a-z0-9-]+`, non-empty.
    let name_is_valid = !name_part.is_empty()
        && name_part
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');

    // Strip ` {#name}` from the text (one space + anchor block).
    // `hash_open - 1` skips the single space before `{`.
    let stripped = &text[..hash_open - 1];

    if !name_is_valid {
        let name_span = Span::new(anchor_span_start + 2, name_part.len().max(1));
        diags.push(
            Diagnostic::new(Code::BadHeadingAnchor, name_span)
                .label("anchor must match `[a-z0-9-]+`")
                .help("use lowercase letters, digits, and hyphens only"),
        );
        return (stripped, None);
    }

    (stripped, Some(name_part.to_string()))
}

/// A block comment opens only on a line that is exactly `/*` or starts with
/// `/* `. Prose and paths that merely begin with the two characters
/// (`/*.tmp files`, `/**`) stay text: an opener swallows every line up to
/// the next `*/`, so a loose rule silently deletes content.
pub(crate) fn opens_block_comment(trimmed: &str) -> bool {
    trimmed == "/*" || trimmed.starts_with("/* ")
}

fn leading_block_sigil(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let b = s.as_bytes()[0];
    if b == b'#' || b == b'>' || b == b'|' || b == b'`' {
        return true;
    }
    if s == "---" {
        return true;
    }
    if s.starts_with("- ") {
        return true;
    }
    if leading_ordered_marker(s).is_some() {
        return true;
    }
    if s.starts_with("//") || opens_block_comment(s) {
        return true;
    }
    if s == "@end" || s.starts_with("@end ") {
        return true;
    }
    if b == b'@' {
        // a leading directive starts a block; non-directive @ inside text wouldn't appear at line start
        return true;
    }
    false
}

fn leading_ordered_marker(s: &str) -> Option<(u32, usize)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    if bytes.get(i) != Some(&b'.') {
        return None;
    }
    if bytes.get(i + 1) != Some(&b' ') {
        return None;
    }
    let n: u32 = s[..i].parse().ok()?;
    Some((n, i + 2))
}

/// Byte offset of `inner` within `outer`, which it must be a slice of.
/// `split_cells` hands back trimmed slices of the row; this recovers where
/// each cell sits so its spans point at the cell, not the row start.
fn subslice_offset(outer: &str, inner: &str) -> usize {
    let off = (inner.as_ptr() as usize).wrapping_sub(outer.as_ptr() as usize);
    debug_assert!(off + inner.len() <= outer.len(), "not a subslice");
    off
}

/// Outcome of splitting a table row.
///
/// `cells` borrow from the slice that was passed in (the `line` argument
/// to `split_cells`). The caller is responsible for providing the line
/// in whatever frame it wants `unclosed_backtick_at` to be relative to —
/// see the doc comment on that field.
#[derive(Debug)]
pub(crate) struct RowSplit<'line> {
    pub(crate) cells: Vec<&'line str>,
    /// `Some(byte_offset)` if a backtick code span opened in this row
    /// and never closed. The offset is relative to **the input slice
    /// passed to `split_cells`** (i.e. the same `&str` whose address
    /// the cell slices are also relative to). Callers that need an
    /// absolute span into the source must add the offset of `line` from
    /// the source start themselves.
    pub(crate) unclosed_backtick_at: Option<usize>,
}

pub(crate) fn split_cells(line: &str) -> RowSplit<'_> {
    let bytes = line.as_bytes();
    // The leading `|` is the row-opener, not a cell separator.
    let body_start = if bytes.first() == Some(&b'|') { 1 } else { 0 };
    let body = &line[body_start..];
    let body_bytes = body.as_bytes();
    let mut cells: Vec<&str> = Vec::new();
    let mut cell_start = 0usize;
    let mut i = 0usize;
    // Offset is relative to `line` (the parameter) — not to `body` —
    // so callers can add `tok.span.start + leading_ws` and get an
    // absolute byte offset into the source map.
    let mut unclosed: Option<usize> = None;
    while i < body_bytes.len() {
        let b = body_bytes[i];
        if b == b'\\' {
            // Escape: skip the backslash and one following byte. The
            // body is later passed to parse_inline which handles UTF-8;
            // here we just need to *not* re-trigger on the escaped byte.
            i += 1;
            if i < body_bytes.len() {
                i += 1;
            }
            continue;
        }
        if b == b'`' {
            // Span length: 1 or 2 backticks. Matches the inline parser
            // (see `inline.rs::parse_code`).
            let ticks = if body_bytes.get(i + 1) == Some(&b'`') {
                2
            } else {
                1
            };
            let span_open = i;
            let needle: &[u8] = if ticks == 2 { b"``" } else { b"`" };
            let mut j = i + ticks;
            let mut closed = false;
            while j + ticks <= body_bytes.len() {
                if &body_bytes[j..j + ticks] == needle {
                    j += ticks;
                    closed = true;
                    break;
                }
                j += 1;
            }
            if !closed {
                // Record the unclosed-backtick offset *relative to
                // `line`* (so we add `body_start`, since `span_open` is
                // relative to `body`). Stop splitting; the caller will
                // emit a single Code::UnterminatedCode diagnostic.
                unclosed = Some(body_start + span_open);
                break;
            }
            i = j;
            continue;
        }
        if b == b'|' {
            cells.push(&body[cell_start..i]);
            cell_start = i + 1;
            i += 1;
            continue;
        }
        i += 1;
    }
    if unclosed.is_none() {
        // Push the final cell. Strip a trailing empty cell only when the
        // *source* used a `|` as a row-closer (cell_start lands right
        // after the trailing `|`, leaving an empty slice).
        let last = &body[cell_start..];
        if !(last.is_empty() && cell_start > 0 && body_bytes[cell_start - 1] == b'|') {
            cells.push(last);
        }
    }
    let trimmed: Vec<&str> = cells.into_iter().map(str::trim).collect();
    RowSplit {
        cells: trimmed,
        unclosed_backtick_at: unclosed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer;

    fn p(s: &str) -> (Document, Vec<Diagnostic>) {
        let src = SourceMap::new("d.brf", s);
        let toks = lexer::lex(&src).unwrap();
        parse(toks, &src)
    }

    #[test]
    fn heading_levels() {
        let (doc, d) = p("# A\n## B\n");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(doc.blocks.len(), 2);
        if let Block::Heading { level, .. } = doc.blocks[0] {
            assert_eq!(level, 1);
        }
        if let Block::Heading { level, .. } = doc.blocks[1] {
            assert_eq!(level, 2);
        }
    }

    #[test]
    fn heading_too_deep() {
        let (_, d) = p("####### x\n");
        assert!(d.iter().any(|x| x.code == Code::HeadingTooDeep));
    }

    #[test]
    fn ordered_sequence() {
        let (_, d) = p("1. one\n3. three\n");
        assert!(d.iter().any(|x| x.code == Code::OrderedListSequence));
    }

    #[test]
    fn ordered_ok() {
        let (doc, d) = p("1. one\n2. two\n");
        assert!(d.is_empty(), "{:?}", d);
        assert!(matches!(doc.blocks[0], Block::List { ordered: true, .. }));
    }

    #[test]
    fn unordered_nested() {
        let (doc, d) = p("- a\n  - a1\n- b\n");
        assert!(d.is_empty(), "{:?}", d);
        if let Block::List { items, .. } = &doc.blocks[0] {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].children.len(), 1);
        } else {
            panic!();
        }
    }

    #[test]
    fn paragraph_join() {
        let (doc, d) = p("one\ntwo\n");
        assert!(d.is_empty());
        if let Block::Paragraph { content, .. } = &doc.blocks[0]
            && let Inline::Text { value, .. } = &content[0]
        {
            assert_eq!(value, "one two");
        }
    }

    #[test]
    fn code_block() {
        let (doc, d) = p("```rust\nfn x() {}\n```\n");
        assert!(d.is_empty(), "{:?}", d);
        if let Block::CodeBlock {
            lang, body, attrs, ..
        } = &doc.blocks[0]
        {
            assert_eq!(lang.as_deref(), Some("rust"));
            assert_eq!(body, "fn x() {}");
            assert_eq!(*attrs, CodeAttrs::default());
        } else {
            panic!();
        }
    }

    #[test]
    fn code_fence_nominify_attr() {
        let (doc, d) = p("```json @nominify\n{\"a\":1}\n```\n");
        assert!(d.is_empty(), "{:?}", d);
        if let Block::CodeBlock { lang, attrs, .. } = &doc.blocks[0] {
            assert_eq!(lang.as_deref(), Some("json"));
            assert!(attrs.nominify);
            assert!(!attrs.minify);
        } else {
            panic!();
        }
    }

    #[test]
    fn code_fence_minify_attr() {
        let (doc, d) = p("```rust @minify\nfn x() {}\n```\n");
        assert!(d.is_empty(), "{:?}", d);
        if let Block::CodeBlock { lang, attrs, .. } = &doc.blocks[0] {
            assert_eq!(lang.as_deref(), Some("rust"));
            assert!(attrs.minify);
        } else {
            panic!();
        }
    }

    #[test]
    fn code_fence_unknown_attr_errors() {
        let (_, d) = p("```json @bogus\n{}\n```\n");
        assert!(
            d.iter().any(|x| x.code == Code::UnknownCodeAttribute),
            "{:?}",
            d
        );
    }

    #[test]
    fn code_fence_attr_without_at_sigil_errors() {
        let (_, d) = p("```json bogus\n{}\n```\n");
        assert!(
            d.iter().any(|x| x.code == Code::UnknownCodeAttribute),
            "{:?}",
            d
        );
    }

    #[test]
    fn code_fence_conflicting_attrs() {
        let (_, d) = p("```json @nominify @minify\n{}\n```\n");
        assert!(
            d.iter().any(|x| x.code == Code::ConflictingCodeAttributes),
            "{:?}",
            d
        );
    }

    #[test]
    fn code_fence_attr_only_no_lang() {
        // An attribute on the first non-whitespace position means the block
        // has no language; the attribute still parses.
        let (doc, d) = p("``` @nominify\nbody\n```\n");
        assert!(d.is_empty(), "{:?}", d);
        if let Block::CodeBlock { lang, attrs, .. } = &doc.blocks[0] {
            assert!(lang.is_none());
            assert!(attrs.nominify);
        } else {
            panic!();
        }
    }

    #[test]
    fn table_basic() {
        let (doc, d) = p("@t\n| A | B\n| 1 | 2\n");
        assert!(d.is_empty(), "{:?}", d);
        if let Block::Table { rows, .. } = &doc.blocks[0] {
            assert_eq!(rows.len(), 1);
        } else {
            panic!("{:?}", doc.blocks);
        }
    }

    #[test]
    fn table_column_mismatch() {
        let (_, d) = p("@t\n| A | B | C\n| 1 | 2\n");
        assert!(d.iter().any(|x| x.code == Code::TableColumnMismatch));
    }

    #[test]
    fn table_pipe_inside_inline_code_span_is_not_a_separator() {
        // Production report issue #2: documenting `|>` should not blow up
        // the row's column count.
        let (doc, d) = p("@t\n| Op | Meaning\n| `|>` | pipeline\n");
        assert!(d.is_empty(), "{:?}", d);
        if let Block::Table { rows, .. } = &doc.blocks[0] {
            assert_eq!(rows.len(), 1, "{:?}", rows);
            assert_eq!(rows[0].cells.len(), 2);
        } else {
            panic!("expected table");
        }
    }

    #[test]
    fn table_pipe_inside_double_backtick_span_is_not_a_separator() {
        let (doc, d) = p("@t\n| A | B\n| ``a ` b | c`` | d\n");
        assert!(d.is_empty(), "{:?}", d);
        if let Block::Table { rows, .. } = &doc.blocks[0] {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].cells.len(), 2);
        } else {
            panic!();
        }
    }

    #[test]
    fn table_unclosed_backtick_in_row_reports_unterminated_code_not_column_mismatch() {
        let (_doc, d) = p("@t\n| A | B\n| `oops | c\n");
        assert!(
            d.iter().any(|x| x.code == Code::UnterminatedCode),
            "{:?}",
            d
        );
        assert!(
            !d.iter().any(|x| x.code == Code::TableColumnMismatch),
            "{:?}",
            d
        );
    }

    #[test]
    fn table_unclosed_backtick_with_indented_row_diagnostic_anchors_correctly() {
        // Two-space-indented `@t` (legal — tables can appear inside
        // indented contexts). Diagnostic offset must include the leading
        // whitespace, not just the post-trim column.
        let (_doc, d) = p("  @t\n  | A | B\n  | `oops | c\n");
        let unterm: Vec<_> = d
            .iter()
            .filter(|x| x.code == Code::UnterminatedCode)
            .collect();
        assert_eq!(unterm.len(), 1, "{:?}", d);
    }

    #[test]
    fn block_shortcode() {
        let (doc, d) = p("@callout(kind: warning)\nbody\n@end\n");
        assert!(d.is_empty(), "{:?}", d);
        assert!(matches!(doc.blocks[0], Block::BlockShortcode { .. }));
    }

    #[test]
    fn hr() {
        let (doc, _) = p("---\n");
        assert!(matches!(doc.blocks[0], Block::HorizontalRule { .. }));
    }

    #[test]
    fn frontmatter_basic() {
        let input = "+++\ntitle = \"hi\"\nn = 3\n+++\n# Doc\n";
        let (doc, d) = p(input);
        assert!(d.is_empty(), "{:?}", d);
        let meta = doc.metadata.as_ref().expect("metadata present");
        assert_eq!(meta.get("title").and_then(|v| v.as_str()), Some("hi"));
        assert_eq!(meta.get("n").and_then(|v| v.as_integer()), Some(3));
        assert_eq!(doc.blocks.len(), 1);
        assert!(matches!(doc.blocks[0], Block::Heading { level: 1, .. }));
    }

    #[test]
    fn frontmatter_empty_table() {
        let (doc, d) = p("+++\n+++\n");
        assert!(d.is_empty(), "{:?}", d);
        let meta = doc.metadata.as_ref().expect("metadata present");
        assert!(meta.is_empty());
        assert!(doc.blocks.is_empty());
    }

    #[test]
    fn frontmatter_unterminated() {
        let (_, d) = p("+++\nfoo = 1\n");
        assert!(
            d.iter().any(|x| x.code == Code::UnterminatedFrontmatter),
            "{:?}",
            d
        );
    }

    #[test]
    fn frontmatter_bad_toml() {
        let (_, d) = p("+++\nfoo === 1\n+++\n");
        assert!(d.iter().any(|x| x.code == Code::FrontmatterToml), "{:?}", d);
    }

    #[test]
    fn frontmatter_only_first_line() {
        // Leading blank line means the document does not start with `+++`,
        // so this is a stray paragraph, not frontmatter.
        let (doc, _d) = p("\n+++\nfoo = 1\n+++\n");
        assert!(doc.metadata.is_none());
    }

    #[test]
    fn frontmatter_indented_is_not_frontmatter() {
        // Leading spaces on the opening line mean it's not a delimiter.
        let (doc, _d) = p("  +++\nfoo = 1\n+++\n");
        assert!(doc.metadata.is_none());
    }

    #[test]
    fn frontmatter_no_open_means_none() {
        let (doc, _d) = p("# Heading\n");
        assert!(doc.metadata.is_none());
    }

    #[test]
    fn frontmatter_crlf() {
        let input = "+++\r\ntitle = \"hi\"\r\n+++\r\n# Doc\r\n";
        let (doc, d) = p(input);
        assert!(d.is_empty(), "{:?}", d);
        let meta = doc.metadata.as_ref().expect("metadata present");
        assert_eq!(meta.get("title").and_then(|v| v.as_str()), Some("hi"));
    }

    #[test]
    fn dl_basic_two_pairs() {
        let (doc, d) = p("@dl\nTerm 1\n: Definition 1.\nTerm 2\n: Definition 2.\n@end\n");
        assert!(d.is_empty(), "{:?}", d);
        let dl = match &doc.blocks[0] {
            Block::DefinitionList { items, .. } => items,
            other => panic!("expected DefinitionList, got {:?}", other),
        };
        assert_eq!(dl.len(), 2);
        let term0 = match &dl[0].term[0] {
            Inline::Text { value, .. } => value.as_str(),
            _ => panic!("expected Text in term"),
        };
        let def0 = match &dl[0].definition[0] {
            Inline::Text { value, .. } => value.as_str(),
            _ => panic!("expected Text in definition"),
        };
        assert_eq!(term0, "Term 1");
        assert_eq!(def0, "Definition 1.");
        let term1 = match &dl[1].term[0] {
            Inline::Text { value, .. } => value.as_str(),
            _ => panic!("expected Text in term"),
        };
        assert_eq!(term1, "Term 2");
    }

    #[test]
    fn dl_continuation_joins_with_space() {
        let input = "@dl\nTerm\n: Definition that\n  spans two lines.\n@end\n";
        let (doc, d) = p(input);
        assert!(d.is_empty(), "{:?}", d);
        let items = match &doc.blocks[0] {
            Block::DefinitionList { items, .. } => items,
            other => panic!("expected DefinitionList, got {:?}", other),
        };
        assert_eq!(items.len(), 1);
        let def_text = match &items[0].definition[0] {
            Inline::Text { value, .. } => value.as_str(),
            _ => panic!("expected Text"),
        };
        assert_eq!(def_text, "Definition that spans two lines.");
    }

    #[test]
    fn dl_definition_without_term_is_b0505() {
        let (_, d) = p("@dl\n: Stray definition.\nTerm\n: Def.\n@end\n");
        assert!(
            d.iter().any(|x| x.code == Code::BadDefinitionList),
            "{:?}",
            d
        );
    }

    #[test]
    fn dl_term_without_definition_is_b0505() {
        let (_, d) = p("@dl\nLonely term\n@end\n");
        assert!(
            d.iter().any(|x| x.code == Code::BadDefinitionList),
            "{:?}",
            d
        );
    }

    #[test]
    fn dl_multiple_definitions_per_term_is_b0505() {
        let (_, d) = p("@dl\nTerm\n: First def.\n: Second def.\n@end\n");
        assert!(
            d.iter().any(|x| x.code == Code::BadDefinitionList),
            "{:?}",
            d
        );
    }

    #[test]
    fn dl_empty_body_is_b0505() {
        let (_, d) = p("@dl\n@end\n");
        assert!(
            d.iter().any(|x| x.code == Code::BadDefinitionList),
            "{:?}",
            d
        );
    }

    #[test]
    fn dl_unterminated_is_b0306() {
        let (_, d) = p("@dl\nTerm\n: Def.\n");
        assert!(
            d.iter().any(|x| x.code == Code::UnterminatedBlock),
            "{:?}",
            d
        );
    }

    #[test]
    fn deep_list_nesting_is_b0318_not_crash() {
        let src: String = (0..6000)
            .map(|i| format!("{}- x\n", "  ".repeat(i)))
            .collect();
        let (_, d) = p(&src);
        assert!(d.iter().any(|x| x.code == Code::NestingTooDeep), "{:?}", d);
    }

    #[test]
    fn deep_shortcode_nesting_is_b0318_not_crash() {
        let src = format!("{}x\n{}", "@details\n".repeat(6000), "@end\n".repeat(6000));
        let (_, d) = p(&src);
        assert!(d.iter().any(|x| x.code == Code::NestingTooDeep), "{:?}", d);
    }

    #[test]
    fn deep_blockquote_markers_is_b0318_not_crash() {
        let src = format!("{} hi\n", ">".repeat(50_000));
        let (_, d) = p(&src);
        assert!(d.iter().any(|x| x.code == Code::NestingTooDeep), "{:?}", d);
    }

    #[test]
    fn code_shortcode_body_is_verbatim() {
        let (doc, d) = p("@code(lang: rust)\nlet x = *ptr;\n@end\n");
        assert!(d.is_empty(), "{:?}", d);
        match &doc.blocks[0] {
            Block::CodeBlock { lang, body, .. } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert_eq!(body, "let x = *ptr;");
            }
            b => panic!("expected CodeBlock, got {:?}", b),
        }
    }

    #[test]
    fn code_shortcode_positional_lang_and_nested_fence() {
        let (doc, d) = p("@code(brief)\n```rust\nfn main() {}\n```\n@end\n");
        assert!(d.is_empty(), "{:?}", d);
        match &doc.blocks[0] {
            Block::CodeBlock { lang, body, .. } => {
                assert_eq!(lang.as_deref(), Some("brief"));
                assert_eq!(body, "```rust\nfn main() {}\n```");
            }
            b => panic!("expected CodeBlock, got {:?}", b),
        }
    }

    #[test]
    fn code_shortcode_unknown_arg_is_b0406() {
        let (_, d) = p("@code(bogus: 1)\nx\n@end\n");
        assert!(d.iter().any(|x| x.code == Code::BadArgSyntax), "{:?}", d);
    }

    #[test]
    fn code_shortcode_unterminated_is_b0306() {
        let (_, d) = p("@code\nx\n");
        assert!(
            d.iter().any(|x| x.code == Code::UnterminatedBlock),
            "{:?}",
            d
        );
    }

    #[test]
    fn table_align_value_outside_enum_is_b0404() {
        let (_, d) = p("@t(align: [\"left\\\"><script>x</script>\", right])\n| A | B\n| 1 | 2\n");
        assert!(d.iter().any(|x| x.code == Code::BadEnumValue), "{:?}", d);
    }

    #[test]
    fn table_align_valid_values_ok() {
        let (_, d) = p("@t(align: [left, right, center])\n| A | B | C\n| 1 | 2 | 3\n");
        assert!(d.is_empty(), "{:?}", d);
    }

    #[test]
    fn four_dash_rule_is_b0304() {
        let (_, d) = p("----\n");
        assert!(
            d.iter().any(|x| x.code == Code::BadHorizontalRule),
            "{:?}",
            d
        );
    }

    #[test]
    fn two_dash_rule_is_b0304() {
        let (_, d) = p("--\n");
        assert!(
            d.iter().any(|x| x.code == Code::BadHorizontalRule),
            "{:?}",
            d
        );
    }

    #[test]
    fn three_dash_rule_is_ok() {
        let (doc, d) = p("---\n");
        assert!(d.is_empty(), "{:?}", d);
        assert!(matches!(doc.blocks[0], Block::HorizontalRule { .. }));
    }

    #[test]
    fn dl_extra_space_after_colon_is_b0505() {
        let (_, d) = p("@dl\nTerm\n:  two spaces\n@end\n");
        assert!(
            d.iter().any(|x| x.code == Code::BadDefinitionList),
            "{:?}",
            d
        );
    }

    #[test]
    fn dl_no_space_after_colon_is_b0505() {
        let (_, d) = p("@dl\nTerm\n:nospace\n@end\n");
        assert!(
            d.iter().any(|x| x.code == Code::BadDefinitionList),
            "{:?}",
            d
        );
    }

    /// (code, line, column) of each diagnostic, in emission order.
    fn at(s: &str) -> Vec<(Code, usize, usize)> {
        let src = SourceMap::new("d.brf", s);
        let (_, d) = parse(lexer::lex(&src).unwrap(), &src);
        d.iter()
            .map(|x| {
                let pos = src.line_column(x.span.start);
                (x.code, pos.line, pos.column)
            })
            .collect()
    }

    fn code_body(doc: &Document) -> &str {
        match &doc.blocks[0] {
            Block::CodeBlock { body, .. } => body,
            b => panic!("expected CodeBlock, got {:?}", b),
        }
    }

    #[test]
    fn line_starting_with_slash_star_is_not_a_comment() {
        // This used to open a comment and delete everything up to the
        // `End */` line with exit 0. The stars are now ordinary inline
        // markers (unclosed, so B0207) and every block survives.
        let (doc, d) =
            p("Delete with:\n/*.tmp files are temp\n\nNext.\n\n## Section\n\nEnd */\n\nfinal\n");
        assert_eq!(doc.blocks.len(), 5, "{:?}", doc.blocks);
        assert!(matches!(doc.blocks[2], Block::Heading { level: 2, .. }));
        let codes: Vec<_> = d.iter().map(|x| x.code).collect();
        assert_eq!(codes, vec![Code::UnterminatedEmph, Code::UnterminatedEmph]);
        assert_eq!(
            at("Delete with:\n/*.tmp files are temp\n\nEnd */\n"),
            vec![
                (Code::UnterminatedEmph, 2, 2),
                (Code::UnterminatedEmph, 4, 5)
            ]
        );

        let (doc, d) = p("/** doc-comment style prose\n\n## Section\n");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(doc.blocks.len(), 2);
    }

    #[test]
    fn block_comment_opens_on_bare_or_spaced_marker() {
        let (doc, d) = p("/*\nhidden\n*/\n/* one line */\n  /* indented\n  */\nshown\n");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(doc.blocks.len(), 1);
        match &doc.blocks[0] {
            Block::Paragraph { content, .. } => {
                assert!(matches!(&content[0], Inline::Text { value, .. } if value == "shown"));
            }
            b => panic!("{:?}", b),
        }
    }

    #[test]
    fn unterminated_comment_is_b0319_at_its_opener() {
        assert_eq!(
            at("a\n\n/* open\n\nmore\n"),
            vec![(Code::UnterminatedComment, 3, 1)]
        );
        assert_eq!(at("- x\n\n  /*\n"), vec![(Code::UnterminatedComment, 3, 3)]);
    }

    #[test]
    fn comment_opened_after_content_is_b0307() {
        // A `/*` ending a content line does not open a comment, so the
        // "hidden" text would be published; flag the pair instead.
        assert_eq!(
            at("text /*\nhidden\n*/\n"),
            vec![(Code::InlineBlockComment, 1, 6)]
        );
    }

    #[test]
    fn dl_stray_indented_line_is_b0505() {
        assert_eq!(
            at("@dl\nTerm\n: def\n    weird indented\n@end\n"),
            vec![(Code::BadDefinitionList, 4, 1)]
        );
        // Continuation indent with no definition to continue, before a
        // definition and after a blank line ended one.
        assert_eq!(
            at("@dl\nTerm\n  orphan\n: def\n\n  more\n@end\n"),
            vec![
                (Code::BadDefinitionList, 3, 1),
                (Code::BadDefinitionList, 6, 1)
            ]
        );
    }

    #[test]
    fn dl_dedent_without_end_is_b0306_and_keeps_the_line() {
        let src = "- item\n\n  @dl\n  T\n  : d\nafter\n";
        assert_eq!(at(src), vec![(Code::UnterminatedBlock, 3, 1)]);
        let (doc, _) = p(src);
        assert_eq!(doc.blocks.len(), 2, "{:?}", doc.blocks);
        assert!(matches!(doc.blocks[1], Block::Paragraph { .. }));
    }

    #[test]
    fn code_fence_body_is_byte_exact() {
        let (doc, d) = p("```go\nfunc main() {\n\tfmt.Println()  \n   \n}\n```\n");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(code_body(&doc), "func main() {\n\tfmt.Println()  \n   \n}");

        let (doc, d) = p("```\r\nx \t\r\n```\r\n");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(code_body(&doc), "x \t");
    }

    #[test]
    fn code_shortcode_body_is_byte_exact() {
        let (doc, d) = p("@code(make)\nall:\n\tcc x.c \n@end\n");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(code_body(&doc), "all:\n\tcc x.c ");
    }

    #[test]
    fn tab_in_frontmatter_is_allowed() {
        let (doc, d) = p("+++\ntitle =\t\"x\"\n+++\n# T\n");
        assert!(d.is_empty(), "{:?}", d);
        assert!(doc.metadata.is_some());
    }

    #[test]
    fn tab_outside_verbatim_regions_is_b0102() {
        assert_eq!(
            at("# T\n\nsome\ttext\n\n```\n\tok\n```\n\n\tindented\n@code\n\tok\n@end\n"),
            vec![(Code::TabCharacter, 3, 5), (Code::TabCharacter, 9, 1)]
        );
    }

    #[test]
    fn inline_arg_error_reports_its_real_position() {
        // Was reported at 3:5: the offset within the line, used as an
        // offset into the file.
        assert_eq!(
            at("# Title\n\nSome intro paragraph.\n\nPress @kbd(1 2)[x] now.\n"),
            vec![(Code::BadArgSyntax, 5, 14)]
        );
    }

    #[test]
    fn joined_paragraph_spans_survive_crlf_indent_trailing_space_and_breaks() {
        let src = "- item\n\n  first é  \r\n  second \\\r\n  third @kbd(1 2)[x]\n";
        assert_eq!(at(src), vec![(Code::BadArgSyntax, 5, 16)]);
        let (doc, _) = p(src);
        let Block::List { items, .. } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks);
        };
        let Block::Paragraph { content, .. } = &items[0].children[0] else {
            panic!("{:?}", items[0].children);
        };
        let slices: Vec<&str> = content
            .iter()
            .map(|n| match n {
                Inline::Text { span, .. } | Inline::HardBreak { span } => &src[span.range()],
                other => panic!("{:?}", other),
            })
            .collect();
        assert_eq!(
            slices,
            vec![
                "first é  \r\n  second ",
                "\\",
                "\r\n  third ",
                "@kbd(1 ",
                "2)[x]"
            ]
        );
    }

    #[test]
    fn table_cell_spans_point_into_the_cell() {
        let src = "@t\n| A | B\n| x | @kbd(é)[y]\n";
        assert_eq!(at(src), vec![(Code::BadArgSyntax, 3, 12)]);
        let s = SourceMap::new("d.brf", src);
        let (_, d) = parse(lexer::lex(&s).unwrap(), &s);
        assert_eq!(&src[d[0].span.range()], "é");
    }

    #[test]
    fn blockquote_keeps_inline_diagnostics() {
        let src = "> fine\n> *unclosed bold\n";
        assert_eq!(at(src), vec![(Code::UnterminatedEmph, 2, 3)]);
        let (doc, _) = p(src);
        let Block::Blockquote { children, .. } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks);
        };
        let Block::Paragraph { content, .. } = &children[1] else {
            panic!("{:?}", children);
        };
        let values: Vec<&str> = content
            .iter()
            .map(|n| match n {
                Inline::Text { value, .. } => value.as_str(),
                other => panic!("{:?}", other),
            })
            .collect();
        assert_eq!(values, vec!["*", "unclosed bold"]);
    }

    #[test]
    fn dl_continuation_spans_map_to_their_lines() {
        assert_eq!(
            at("@dl\nTerm\n: first line\n  second @kbd(1 2)[x]\n@end\n"),
            vec![(Code::BadArgSyntax, 4, 17)]
        );
    }

    #[test]
    fn directive_arg_errors_are_absolute_and_whole_chars() {
        let src = "# T\n\n@callout(kind: é)\nx\n@end\n";
        assert_eq!(at(src), vec![(Code::BadArgSyntax, 3, 16)]);
        let s = SourceMap::new("d.brf", src);
        let (_, d) = parse(lexer::lex(&s).unwrap(), &s);
        assert_eq!(&src[d[0].span.range()], "é");
        assert_eq!(
            at("x\n\n@t(align: é)\n| A\n"),
            vec![(Code::BadArgSyntax, 3, 11)]
        );
        assert_eq!(
            at("x\n\n@dl(a: 1, a: 2)\nT\n: d\n@end\n"),
            vec![(Code::DuplicateKwarg, 3, 11)]
        );
    }

    #[test]
    fn block_math_body_is_verbatim_text() {
        let (doc, d) = p("@math\n\\frac{a_1}{b} *x*\n\\\\ y\n@end\n");
        assert!(d.is_empty(), "{:?}", d);
        let Block::BlockShortcode { name, children, .. } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks);
        };
        assert_eq!(name, "math");
        let [Block::Paragraph { content, .. }] = children.as_slice() else {
            panic!("{:?}", children);
        };
        assert!(
            matches!(content.as_slice(), [Inline::Text { value, .. }] if value == "\\frac{a_1}{b} *x*\n\\\\ y"),
            "{:?}",
            content
        );
    }

    #[test]
    fn nesting_at_limit_is_ok() {
        let src: String = (0..MAX_NESTING_DEPTH)
            .map(|i| format!("{}- x\n", "  ".repeat(i)))
            .collect();
        let (_, d) = p(&src);
        assert!(d.is_empty(), "{:?}", d);
    }
}
