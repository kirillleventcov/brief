use crate::ast::{Block, CodeAttrs, Document, Inline, ListItem, Row, ShortArgs, TaskState};
use crate::diag::{Code, Diagnostic};
use crate::inline::{parse_args, parse_inline};
use crate::span::{SourceMap, Span};
use crate::token::{Token, TokenKind};

pub fn parse(tokens: Vec<Token>, src: &SourceMap) -> (Document, Vec<Diagnostic>) {
    let (metadata, fm_consumed, fm_diags) = parse_frontmatter(&tokens, src);
    let mut p = Parser {
        src,
        toks: tokens,
        pos: fm_consumed,
        diags: fm_diags,
        depth: 0,
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
    (
        Document {
            blocks,
            metadata,
            resolved_refs: Default::default(),
        },
        p.diags,
    )
}

fn parse_frontmatter(
    toks: &[Token],
    src: &SourceMap,
) -> (Option<toml::Table>, usize, Vec<Diagnostic>) {
    // Returns (metadata, tokens_consumed, diagnostics).
    //
    // Frontmatter must be the very first content. We detect it by checking
    // that the first token is `Line("+++")` at byte offset 0 with indent 0.
    // If anything else comes first (a Blank, a comment, an indented `+++`,
    // or a non-`+++` line), there is no frontmatter and we return
    // (None, 0, empty).
    if toks.is_empty() {
        return (None, 0, Vec::new());
    }
    let first = &toks[0];
    let opens = match first.kind {
        TokenKind::Line => {
            first.indent == 0 && first.span.start == 0 && &src.source[first.span.range()] == "+++"
        }
        _ => false,
    };
    if !opens {
        return (None, 0, Vec::new());
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
                return (None, idx, diags);
            }
            TokenKind::Line
                if toks[idx].indent == 0 && &src.source[toks[idx].span.range()] == "+++" =>
            {
                let close = &toks[idx];
                let body_end = close.span.start as usize;
                let body = &src.source[body_start..body_end];
                idx += 1; // consume the closing `+++`
                match toml::from_str::<toml::Table>(body) {
                    Ok(t) => return (Some(t), idx, diags),
                    Err(e) => {
                        let (off, len) = match e.span() {
                            Some(r) => (body_start + r.start, r.end - r.start),
                            None => (body_start, body.len()),
                        };
                        // `.max(1)` keeps the diagnostic caret renderable; a
                        // zero-length span produces no caret in the error UI.
                        let span = Span::new(off, len.max(1));
                        diags.push(
                            Diagnostic::new(Code::FrontmatterToml, span).label(e.to_string()),
                        );
                        return (None, idx, diags);
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
    (None, idx, diags)
}

/// Blocks may nest via list children, block shortcodes, and blockquote
/// markers. Each level recurses on the native stack, so nesting must be
/// bounded or hostile/degenerate input aborts the process with a stack
/// overflow instead of a diagnostic.
const MAX_NESTING_DEPTH: usize = 64;

struct Parser<'a> {
    src: &'a SourceMap,
    toks: Vec<Token>,
    pos: usize,
    diags: Vec<Diagnostic>,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Token {
        &self.toks[self.pos]
    }
    /// Text of a `Line` token, sliced from the source. The returned slice
    /// borrows from the `SourceMap` (lifetime `'a`), not from `self`, so it
    /// stays valid across `&mut self` calls.
    fn line(&self, tok: &Token) -> &'a str {
        &self.src.source[tok.span.range()]
    }
    fn at_eof(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
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
                    if let Some(min) = end_at_indent_below {
                        if indent < min {
                            break;
                        }
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
                    if let Some(min) = end_at_indent_below {
                        if indent < min {
                            break;
                        }
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
                    if trimmed.starts_with("/*") {
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
        self.pos += 1;
        loop {
            let tok = *self.peek();
            match tok.kind {
                TokenKind::Eof => {
                    self.diags.push(
                        Diagnostic::new(Code::UnterminatedBlock, tok.span)
                            .label("unterminated /* */ comment"),
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

        let (content, idiags) = parse_inline(heading_text, tok.span.start + text_offset as u32);
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
        let mut single: Option<&'a str> = None;
        let mut hard_break_indices: Vec<usize> = Vec::new();
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
                    if first_line && !hard {
                        single = Some(line_text);
                    } else {
                        if let Some(prev) = single.take() {
                            text.push_str(prev);
                        }
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        if hard {
                            hard_break_indices.push(text.len() + line_text.len());
                        }
                        text.push_str(line_text);
                    }
                    first_line = false;
                    span = span.join(tok.span);
                    self.pos += 1;
                }
                _ => break,
            }
        }
        if let Some(s) = single {
            let base = first.span.start + first.indent as u32;
            let (content, d) = parse_inline(s, base);
            self.diags.extend(d);
            return Block::Paragraph { content, span };
        }
        let mut content: Vec<Inline> = Vec::new();
        let mut cursor = 0usize;
        let base = first.span.start + first.indent as u32;
        for hb in &hard_break_indices {
            let chunk = &text[cursor..*hb];
            let (mut inl, d) = parse_inline(chunk, base + cursor as u32);
            self.diags.extend(d);
            content.append(&mut inl);
            content.push(Inline::HardBreak {
                span: Span::new(base as usize + *hb, 1),
            });
            cursor = *hb;
        }
        let chunk = &text[cursor..];
        let (mut inl, d) = parse_inline(chunk, base + cursor as u32);
        self.diags.extend(d);
        content.append(&mut inl);
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
                    break;
                }
                TokenKind::Blank => {
                    body.push('\n');
                    span = span.join(tok.span);
                    self.pos += 1;
                }
                TokenKind::Line => {
                    let s = self.line(&tok);
                    if s.trim() == "```" {
                        span = span.join(tok.span);
                        self.pos += 1;
                        break;
                    }
                    body.push_str(s);
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
            let (content, d) = parse_inline(
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
            let (content, d) = parse_inline(
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
        let mut lines: Vec<(usize, &'a str, Span)> = Vec::new();
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
            let body = &trimmed[idx + 1..];
            lines.push((depth, body, tok.span));
            self.pos += 1;
        }
        let (children, span) = build_blockquote(&lines, 1);
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
            match parse_args(trimmed, &mut cursor) {
                Ok(a) => a,
                Err(d) => {
                    self.diags.push(d);
                    ShortArgs::default()
                }
            }
        } else {
            ShortArgs::default()
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
            let cells = split.cells;
            let mut parsed_cells: Vec<Vec<Inline>> = Vec::new();
            for c in cells {
                let (inl, d) = parse_inline(c.trim(), tok.span.start);
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
        if let Some(crate::shortcode::ArgValue::Array(a)) = args.keyword.get("align") {
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
        use crate::ast::DefinitionItem;
        let directive = *self.peek();
        let line = self.line(&directive);
        self.pos += 1;
        let trimmed = &line[indent as usize..];
        let mut cursor = 3usize;
        let args = if trimmed.as_bytes().get(cursor) == Some(&b'(') {
            match parse_args(trimmed, &mut cursor) {
                Ok(a) => a,
                Err(d) => {
                    self.diags.push(d);
                    ShortArgs::default()
                }
            }
        } else {
            ShortArgs::default()
        };

        let mut items: Vec<DefinitionItem> = Vec::new();
        let mut pending_term: Option<(Vec<Inline>, Span)> = None;
        // (text accumulator, base offset of first line, span covering the
        // definition's lines).
        let mut pending_def: Option<(String, u32, Span)> = None;
        let cont_indent = indent + 2;
        let mut end_span = directive.span;

        // Closes any open definition into items, paired with the pending term.
        let finalize_def = |items: &mut Vec<DefinitionItem>,
                            pending_term: &mut Option<(Vec<Inline>, Span)>,
                            pending_def: &mut Option<(String, u32, Span)>,
                            diags: &mut Vec<Diagnostic>| {
            if let Some((text, base, span)) = pending_def.take() {
                let term_pair = pending_term.take();
                let (def_inl, dd) = parse_inline(&text, base);
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
                        pd.0.push(' ');
                        pd.0.push_str(body);
                        pd.2 = pd.2.join(tok.span);
                        self.pos += 1;
                        continue;
                    }
                    if tok.indent != indent {
                        // Anything else not at @dl's indent inside a `@dl`
                        // body is unexpected — skip it; subsequent tasks
                        // refine error reporting.
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
                                    "multiple definitions per term are not supported in v0.3",
                                ),
                            );
                            // Drop the duplicate definition: do not consume
                            // pending_term, do not start a new pending_def.
                            self.pos += 1;
                            continue;
                        }
                        let base = tok.span.start + indent as u32 + 2;
                        pending_def = Some((rest.to_string(), base, tok.span));
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
                        let (term, td) = parse_inline(body, base);
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
        let mut args = ShortArgs::default();
        if bytes.get(cursor) == Some(&b'(') {
            match parse_args(trimmed, &mut cursor) {
                Ok(a) => args = a,
                Err(d) => self.diags.push(d),
            }
        }
        if !trimmed[cursor..].trim().is_empty() {
            return None;
        }
        self.pos += 1;
        if name == "code" {
            return Some(self.parse_code_shortcode_body(indent, &tok, args));
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
    fn parse_code_shortcode_body(&mut self, indent: u16, open: &Token, args: ShortArgs) -> Block {
        let lang = self.code_shortcode_lang(args, open.span);
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
                    break;
                }
                TokenKind::Blank => {
                    body.push('\n');
                    span = span.join(tok.span);
                    self.pos += 1;
                }
                TokenKind::Line => {
                    let s = self.line(&tok);
                    if s.trim() == "@end" && tok.indent == indent {
                        span = span.join(tok.span);
                        self.pos += 1;
                        break;
                    }
                    body.push_str(s);
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

    /// `@code` becomes a `CodeBlock` before the resolver ever sees it, so its
    /// arguments must be checked here: one optional `lang`, positional or
    /// keyword, string or ident.
    fn code_shortcode_lang(&mut self, args: ShortArgs, span: Span) -> Option<String> {
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

fn build_blockquote(items: &[(usize, &str, Span)], depth: usize) -> (Vec<Block>, Span) {
    let mut paras: Vec<Block> = Vec::new();
    let mut full_span = Span::DUMMY;
    let mut i = 0;
    while i < items.len() {
        let (d, body, span) = &items[i];
        if *d < depth {
            break;
        }
        full_span = if full_span == Span::DUMMY {
            *span
        } else {
            full_span.join(*span)
        };
        if *d == depth {
            let (content, _) = parse_inline(body, span.start);
            paras.push(Block::Paragraph {
                content,
                span: *span,
            });
            i += 1;
        } else {
            let mut j = i;
            while j < items.len() && items[j].0 > depth {
                j += 1;
            }
            let (children, child_span) = build_blockquote(&items[i..j], depth + 1);
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
                        .help("v0.3 supports `@nominify`, `@minify`, `@minify-keep-comments`"),
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
    // Quick exit: if there's no `{#` anywhere, no anchor syntax is possible.
    if !text.contains("{#") {
        return (text, None);
    }

    // Find the last `{#` occurrence (there can be at most one anchor block).
    let hash_open = match text.rfind("{#") {
        Some(i) => i,
        None => return (text, None),
    };

    // Check if the `}` at end-of-line closes this `{#`.
    // Case A: `{#name}` at end of line (the only valid form).
    // Case B: `{#name}` NOT at end of line (content after `}`) → malformed.
    // Case C: no `}` after the `{#` at all → the `{#` is inside text, leave alone.

    let after_hash = &text[hash_open..];
    let rbrace = match after_hash.find('}') {
        Some(i) => i,
        None => {
            // `{#` with no closing `}` anywhere → plain text, no error.
            return (text, None);
        }
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

    let malformed = if before.is_empty() {
        // No content before `{#...}` → no space before `{`.
        true
    } else {
        let last_ch = before.chars().last().unwrap();
        if last_ch != ' ' {
            // No space before `{`
            true
        } else {
            // Check for double space (before ends with "  ")
            let before_trim = &before[..before.len() - 1];
            before_trim.ends_with(' ')
        }
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
    if s.starts_with("//") || s.starts_with("/*") {
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

/// Outcome of splitting a table row.
///
/// `cells` borrow from the slice that was passed in (the `line` argument
/// to `split_cells`). The caller is responsible for providing the line
/// in whatever frame it wants `unclosed_backtick_at` to be relative to —
/// see the doc comment on that field.
#[derive(Debug)]
struct RowSplit<'a> {
    cells: Vec<&'a str>,
    /// `Some(byte_offset)` if a backtick code span opened in this row
    /// and never closed. The offset is relative to **the input slice
    /// passed to `split_cells`** (i.e. the same `&str` whose address
    /// the cell slices are also relative to). Callers that need an
    /// absolute span into the source must add the offset of `line` from
    /// the source start themselves.
    unclosed_backtick_at: Option<usize>,
}

fn split_cells(line: &str) -> RowSplit<'_> {
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
    use crate::lexer::lex;

    fn p(s: &str) -> (Document, Vec<Diagnostic>) {
        let src = SourceMap::new("d.brf", s);
        let toks = lex(&src).unwrap();
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
        if let Block::Paragraph { content, .. } = &doc.blocks[0] {
            if let Inline::Text { value, .. } = &content[0] {
                assert_eq!(value, "one two");
            }
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
        if let crate::ast::Block::Table { rows, .. } = &doc.blocks[0] {
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
        if let crate::ast::Block::Table { rows, .. } = &doc.blocks[0] {
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

    #[test]
    fn nesting_at_limit_is_ok() {
        let src: String = (0..MAX_NESTING_DEPTH)
            .map(|i| format!("{}- x\n", "  ".repeat(i)))
            .collect();
        let (_, d) = p(&src);
        assert!(d.is_empty(), "{:?}", d);
    }
}
