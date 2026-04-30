//! Markdown-to-Brief converter.
//!
//! Walks a `pulldown-cmark` event stream and emits Brief source text. Every
//! Markdown construct that has no clean Brief equivalent is reported as a
//! `Diag` carrying a `Hole` code; nothing is silently dropped.

#[derive(Clone, Debug)]
pub struct ConvertResult {
    pub brief_source: String,
    pub diagnostics: Vec<Diag>,
}

#[derive(Clone, Debug)]
pub struct Diag {
    pub hole: Hole,
    pub line: usize, // 1-indexed
    pub col: usize,  // 1-indexed
    pub original: String,
    pub note: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Hole {
    SetextHeading,
    DoubleEmphasis,   // **x**, __x__, ~~x~~
    AsteriskEmphasis, // *em* (Markdown italic)
    AltBullet,        // * or + bullet
    OrderedRenumber,
    NestIndentNormalize,
    TildeFence,
    IndentedCodeBlock,
    AltHorizontalRule,
    LinkTitleDropped,
    AutolinkRewrap,
    RefLinkInlined,
    GfmAlert,
    InlineHtml,
    HtmlBlock,
    Frontmatter,
    HtmlEntity,
}

impl Hole {
    /// Stable kebab-case slug used in TODO comments and stderr report lines.
    pub fn slug(self) -> &'static str {
        match self {
            Hole::SetextHeading => "setext-heading",
            Hole::DoubleEmphasis => "double-emphasis",
            Hole::AsteriskEmphasis => "asterisk-emphasis",
            Hole::AltBullet => "alt-bullet",
            Hole::OrderedRenumber => "ordered-renumber",
            Hole::NestIndentNormalize => "nest-indent-normalize",
            Hole::TildeFence => "tilde-fence",
            Hole::IndentedCodeBlock => "indented-code-block",
            Hole::AltHorizontalRule => "alt-horizontal-rule",
            Hole::LinkTitleDropped => "link-title-dropped",
            Hole::AutolinkRewrap => "autolink-rewrap",
            Hole::RefLinkInlined => "ref-link-inlined",
            Hole::GfmAlert => "gfm-alert",
            Hole::InlineHtml => "inline-html",
            Hole::HtmlBlock => "html-block",
            Hole::Frontmatter => "frontmatter",
            Hole::HtmlEntity => "html-entity",
        }
    }

    /// Short human-readable label for stderr.
    pub fn message(self) -> &'static str {
        match self {
            Hole::SetextHeading => "setext heading rewritten to ATX",
            Hole::DoubleEmphasis => "doubled emphasis marker rewritten to single",
            Hole::AsteriskEmphasis => "Markdown `*italic*` rewritten to Brief `_italic_`",
            Hole::AltBullet => "`*`/`+` bullet rewritten to `-`",
            Hole::OrderedRenumber => "ordered list renumbered to sequential 1,2,3,...",
            Hole::NestIndentNormalize => "list nesting indent normalized to 2 spaces",
            Hole::TildeFence => "`~~~` fence rewritten to triple-backtick fence",
            Hole::IndentedCodeBlock => "indented code block rewritten to fenced block",
            Hole::AltHorizontalRule => "`***`/`___`/spaced rule rewritten to `---`",
            Hole::LinkTitleDropped => "link/image title attribute dropped",
            Hole::AutolinkRewrap => "autolink/bare URL wrapped in `@link[...](...)`",
            Hole::RefLinkInlined => "reference-style link resolved inline",
            Hole::GfmAlert => "GFM alert blockquote rewritten to `@callout`",
            Hole::InlineHtml => "inline HTML preserved as TODO comment",
            Hole::HtmlBlock => "HTML block preserved inside Brief block comment",
            Hole::Frontmatter => "frontmatter dropped, replaced with TODO comment",
            Hole::HtmlEntity => "HTML entity decoded to literal character",
        }
    }
}

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// Convert a Markdown document to Brief source.
///
/// `source_path` is used only for diagnostic location strings.
pub fn convert(input: &str, source_path: &str) -> ConvertResult {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_GFM);
    opts.insert(Options::ENABLE_MATH);
    opts.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    opts.insert(Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS);

    let line_offsets = compute_line_offsets(input);
    let events: Vec<(Event<'_>, std::ops::Range<usize>)> =
        Parser::new_ext(input, opts).into_offset_iter().collect();

    // Pass 1: render each footnote definition's body to Brief using a fresh
    // sub-Walker so inline formatting (emphasis, links, code, ...) inside the
    // body is preserved. Markdown footnote definitions live elsewhere in the
    // source; Brief's `@footnote[body]` is inline at the reference site, so
    // we must have the rendered body ready before pass 2 visits the
    // FootnoteReference event.
    let mut footnote_defs: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let mut footnote_diags: Vec<Diag> = Vec::new();
    {
        let mut current_label: Option<String> = None;
        let mut current_events: Vec<(Event<'_>, std::ops::Range<usize>)> = Vec::new();
        for (event, range) in &events {
            match event {
                Event::Start(Tag::FootnoteDefinition(label)) => {
                    current_label = Some(label.to_string());
                    current_events.clear();
                }
                Event::End(TagEnd::FootnoteDefinition) => {
                    if let Some(label) = current_label.take() {
                        let mut sub = Walker::new(input, source_path, line_offsets.clone());
                        for (e, r) in current_events.drain(..) {
                            sub.visit(e, r);
                        }
                        let body = sub.out.trim().to_string();
                        footnote_diags.append(&mut sub.diags);
                        footnote_defs.insert(label, body);
                    }
                }
                _ => {
                    if current_label.is_some() {
                        current_events.push((event.clone(), range.clone()));
                    }
                }
            }
        }
    }

    // Pass 2: render the main document, skipping footnote definitions —
    // their bodies are now inlined at the FootnoteReference site.
    let mut walker = Walker::new(input, source_path, line_offsets);
    walker.footnote_defs = footnote_defs;
    walker.diags.extend(footnote_diags);
    let mut skip_until_footnote_end = false;
    for (event, range) in events {
        match (&event, skip_until_footnote_end) {
            (Event::Start(Tag::FootnoteDefinition(_)), _) => {
                skip_until_footnote_end = true;
                continue;
            }
            (Event::End(TagEnd::FootnoteDefinition), _) => {
                skip_until_footnote_end = false;
                continue;
            }
            (_, true) => continue,
            _ => {}
        }
        walker.visit(event, range);
    }
    walker.finish()
}

struct Walker<'a> {
    src: &'a str,
    _source_path: String,
    line_offsets: Vec<usize>,
    out: String,
    diags: Vec<Diag>,
    /// True while inside a paragraph (between Start(Paragraph) and End).
    in_paragraph: bool,
    /// Set while we are inside a fenced/indented code block. Suppresses
    /// inline parsing of `Event::Text` (text inside a code block is verbatim).
    in_code_block: bool,
    /// Stack of active lists.
    list_stack: Vec<ListFrame>,
    /// Stack of pending output buffers. The top of the stack is where output
    /// is actually written. On End(BlockQuote/Alert), pop and post-process
    /// (prefix lines or wrap in callout) before appending to the parent.
    out_stack: Vec<String>,
    /// Mirrors `out_stack`; top tells us how to wrap when the buffer is popped.
    container_stack: Vec<Container>,
    /// Active table accumulator (we only ever have one in flight).
    table: Option<TableState>,
    /// Stack of (dest_url, optional diagnostic to emit on End) for active links/images.
    link_stack: Vec<(String, Option<(Hole, String)>)>,
    /// Footnote labels → body text (rendered as a flat string).
    footnote_defs: std::collections::BTreeMap<String, String>,
    /// HTML-derived comment lines pending insertion before the next paragraph.
    pending_html_comments: Vec<String>,
    in_metadata: bool,
    metadata_buf: String,
    metadata_kind: Option<pulldown_cmark::MetadataBlockKind>,
}

struct TableState {
    aligns: Vec<pulldown_cmark::Alignment>,
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    in_cell: bool,
}

struct ListFrame {
    ordered: bool,
    next_index: u64,
    saw_first_item: bool,
    /// Column (1-indexed) where the *content* of items at this level starts.
    /// Used to detect non-2-space nesting in child items.
    item_content_col: usize,
}

enum Container {
    Quote,
    Alert(#[allow(dead_code)] Hole, &'static str),
    LinkPending,
    ImagePending,
    /// Paragraph buffer; on pop we flush pending HTML comments then this content.
    Paragraph,
}

impl<'a> Walker<'a> {
    fn write(&mut self, s: &str) {
        if let Some(t) = self.table.as_mut() {
            if t.in_cell {
                t.current_cell.push_str(s);
                return;
            }
        }
        if let Some(buf) = self.out_stack.last_mut() {
            buf.push_str(s);
        } else {
            self.out.push_str(s);
        }
    }
    fn write_char(&mut self, c: char) {
        if let Some(t) = self.table.as_mut() {
            if t.in_cell {
                t.current_cell.push(c);
                return;
            }
        }
        if let Some(buf) = self.out_stack.last_mut() {
            buf.push(c);
        } else {
            self.out.push(c);
        }
    }
    fn current_ends_with(&self, c: char) -> bool {
        if let Some(t) = self.table.as_ref() {
            if t.in_cell {
                return t.current_cell.ends_with(c);
            }
        }
        if let Some(buf) = self.out_stack.last() {
            buf.ends_with(c)
        } else {
            self.out.ends_with(c)
        }
    }

    fn new(src: &'a str, source_path: &str, line_offsets: Vec<usize>) -> Self {
        Walker {
            src,
            _source_path: source_path.to_string(),
            line_offsets,
            out: String::new(),
            diags: Vec::new(),
            in_paragraph: false,
            in_code_block: false,
            list_stack: Vec::new(),
            out_stack: Vec::new(),
            container_stack: Vec::new(),
            table: None,
            link_stack: Vec::new(),
            footnote_defs: std::collections::BTreeMap::new(),
            pending_html_comments: Vec::new(),
            in_metadata: false,
            metadata_buf: String::new(),
            metadata_kind: None,
        }
    }

    fn visit(&mut self, event: Event<'_>, range: std::ops::Range<usize>) {
        match event {
            Event::Start(Tag::List(start)) => {
                let ordered = start.is_some();
                if let Some(n) = start {
                    if n != 1 {
                        self.push_diag(
                            Hole::OrderedRenumber,
                            range.clone(),
                            format!("ordered list started at {}; renumbered from 1", n),
                        );
                    }
                }
                // Nested list opens inside a parent Item whose text hasn't
                // closed with a newline yet. Ensure one is in place.
                if !self.list_stack.is_empty() && !self.current_ends_with('\n') {
                    self.write_char('\n');
                }
                self.list_stack.push(ListFrame {
                    ordered,
                    next_index: 1, // always renumber from 1 in Brief
                    saw_first_item: false,
                    item_content_col: 0,
                });
            }
            Event::End(TagEnd::List(_)) => {
                self.list_stack.pop();
            }
            Event::Start(Tag::Item) => {
                let depth = self.list_stack.len().saturating_sub(1);
                let (line, col) = self.pos(range.start);
                let _ = line;
                // Check for non-2-space nesting against parent frame.
                if depth > 0 {
                    let parent = &self.list_stack[depth - 1];
                    if parent.item_content_col > 0 {
                        let expected_col = parent.item_content_col;
                        if col != expected_col && col != parent.item_content_col {
                            let already = self
                                .diags
                                .iter()
                                .any(|d| d.hole == Hole::NestIndentNormalize && d.line == line);
                            if !already {
                                self.push_diag(
                                    Hole::NestIndentNormalize,
                                    range.clone(),
                                    format!(
                                        "nesting at col {} normalized to {} (2-space rule)",
                                        col, expected_col
                                    ),
                                );
                            }
                        }
                    }
                }
                let frame_ordered = self
                    .list_stack
                    .last()
                    .expect("Item without enclosing List")
                    .ordered;
                // For unordered lists, detect alt-bullet first (uses src/diags).
                if !frame_ordered {
                    let snippet = self.src.get(range.clone()).unwrap_or("");
                    let first = snippet
                        .as_bytes()
                        .iter()
                        .find(|&&b| b == b'-' || b == b'*' || b == b'+')
                        .copied();
                    if first == Some(b'*') || first == Some(b'+') {
                        self.push_diag(
                            Hole::AltBullet,
                            range.clone(),
                            format!(
                                "`{}` bullet rewritten to `-`",
                                first.map(|b| b as char).unwrap_or('?')
                            ),
                        );
                    }
                }
                let pad: String = " ".repeat(depth * 2);
                self.write(&pad);
                let frame = self
                    .list_stack
                    .last_mut()
                    .expect("Item without enclosing List");
                let marker_len: usize;
                let marker_str: String;
                if frame.ordered {
                    marker_str = format!("{}. ", frame.next_index);
                    marker_len = marker_str.len();
                    frame.next_index += 1;
                } else {
                    marker_str = "- ".to_string();
                    marker_len = 2;
                }
                frame.saw_first_item = true;
                frame.item_content_col = depth * 2 + marker_len + 1;
                self.write(&marker_str);
            }
            Event::End(TagEnd::Item) => {
                if !self.current_ends_with('\n') {
                    self.write_char('\n');
                }
            }
            Event::Start(Tag::Paragraph) => {
                self.in_paragraph = true;
                // Defer paragraph content into a buffer so we can prepend any
                // HTML-comment lines collected from inline-HTML events before
                // emitting the paragraph itself.
                self.out_stack.push(String::new());
                self.container_stack.push(Container::Paragraph);
            }
            Event::End(TagEnd::Paragraph) => {
                self.in_paragraph = false;
                let body = self.out_stack.pop().expect("paragraph buffer");
                let _ = self.container_stack.pop();
                for c in std::mem::take(&mut self.pending_html_comments) {
                    self.write(&c);
                    self.write_char('\n');
                }
                self.write(&body);
                if self.list_stack.is_empty() {
                    self.write_char('\n');
                    self.write_char('\n');
                } else {
                    // Inside a list item — no trailing blank line.
                    self.write_char('\n');
                }
            }
            Event::Start(Tag::Heading { level, .. }) => {
                let n = match level {
                    pulldown_cmark::HeadingLevel::H1 => 1,
                    pulldown_cmark::HeadingLevel::H2 => 2,
                    pulldown_cmark::HeadingLevel::H3 => 3,
                    pulldown_cmark::HeadingLevel::H4 => 4,
                    pulldown_cmark::HeadingLevel::H5 => 5,
                    pulldown_cmark::HeadingLevel::H6 => 6,
                };
                // Detect setext: the source span at `range` does not start with `#`.
                let snippet = self.src.get(range.clone()).unwrap_or("");
                let is_setext = !snippet.trim_start().starts_with('#');
                if is_setext {
                    self.push_diag(
                        Hole::SetextHeading,
                        range.clone(),
                        format!("rewritten to `{} ...`", "#".repeat(n)),
                    );
                }
                for _ in 0..n {
                    self.write_char('#');
                }
                self.write_char(' ');
            }
            Event::End(TagEnd::Heading(_)) => {
                self.write_char('\n');
            }
            Event::Start(Tag::Emphasis) => {
                // *x* (asterisk italic) is a hole; _x_ is clean.
                let first = self.src.as_bytes().get(range.start).copied();
                if first == Some(b'*') {
                    self.push_diag(
                        Hole::AsteriskEmphasis,
                        range.clone(),
                        "Markdown `*italic*` rewritten to Brief `_italic_`".into(),
                    );
                }
                self.write_char('_');
            }
            Event::End(TagEnd::Emphasis) => {
                self.write_char('_');
            }
            Event::Start(Tag::Strong) => {
                self.push_diag(
                    Hole::DoubleEmphasis,
                    range.clone(),
                    "doubled emphasis marker rewritten to single `*`".into(),
                );
                self.write_char('*');
            }
            Event::End(TagEnd::Strong) => {
                self.write_char('*');
            }
            Event::Start(Tag::Strikethrough) => {
                self.push_diag(
                    Hole::DoubleEmphasis,
                    range.clone(),
                    "doubled strikethrough rewritten to single `~`".into(),
                );
                self.write_char('~');
            }
            Event::End(TagEnd::Strikethrough) => {
                self.write_char('~');
            }
            Event::Code(s) => {
                if s.contains('`') {
                    self.write("``");
                    self.write(&s);
                    self.write("``");
                } else {
                    self.write_char('`');
                    self.write(&s);
                    self.write_char('`');
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                use pulldown_cmark::CodeBlockKind;
                self.in_code_block = true;
                match kind {
                    CodeBlockKind::Fenced(lang) => {
                        // Detect tilde fence by inspecting the source span.
                        let snippet = self.src.get(range.clone()).unwrap_or("");
                        if snippet.trim_start().starts_with('~') {
                            self.push_diag(
                                Hole::TildeFence,
                                range.clone(),
                                "`~~~` fence rewritten to ```` ``` ```` fence".into(),
                            );
                        }
                        self.write("```");
                        if !lang.is_empty() {
                            // Brief takes only the first whitespace-separated token.
                            let lang_token = lang.split_whitespace().next().unwrap_or("");
                            self.write(lang_token);
                        }
                        self.write_char('\n');
                    }
                    CodeBlockKind::Indented => {
                        self.push_diag(
                            Hole::IndentedCodeBlock,
                            range.clone(),
                            "indented code block rewritten to fenced block".into(),
                        );
                        self.write("```\n");
                    }
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                self.in_code_block = false;
                if !self.current_ends_with('\n') {
                    self.write_char('\n');
                }
                self.write("```\n");
            }
            Event::Text(t) => {
                if self.in_metadata {
                    self.metadata_buf.push_str(&t);
                    return;
                }
                if self.in_code_block {
                    self.write(&t);
                } else {
                    self.write(&t);
                }
            }
            Event::TaskListMarker(checked) => {
                // v0.4 §4.3: Brief now natively supports `[x]` / `[ ]`
                // as a list-item modifier. The conversion is lossless, so
                // no diagnostic is emitted.
                self.write(if checked { "[x] " } else { "[ ] " });
            }
            Event::Start(Tag::BlockQuote(kind)) => {
                use pulldown_cmark::BlockQuoteKind;
                let container = match kind {
                    None => Container::Quote,
                    Some(BlockQuoteKind::Note) => {
                        self.push_diag(
                            Hole::GfmAlert,
                            range.clone(),
                            "GFM alert mapped to `@callout(kind: note)`".into(),
                        );
                        Container::Alert(Hole::GfmAlert, "note")
                    }
                    Some(BlockQuoteKind::Tip) => {
                        self.push_diag(
                            Hole::GfmAlert,
                            range.clone(),
                            "GFM alert mapped to `@callout(kind: tip)`".into(),
                        );
                        Container::Alert(Hole::GfmAlert, "tip")
                    }
                    Some(BlockQuoteKind::Important) => {
                        self.push_diag(
                            Hole::GfmAlert,
                            range.clone(),
                            "GFM alert mapped to `@callout(kind: important)`".into(),
                        );
                        Container::Alert(Hole::GfmAlert, "important")
                    }
                    Some(BlockQuoteKind::Warning) => {
                        self.push_diag(
                            Hole::GfmAlert,
                            range.clone(),
                            "GFM alert mapped to `@callout(kind: warning)`".into(),
                        );
                        Container::Alert(Hole::GfmAlert, "warning")
                    }
                    Some(BlockQuoteKind::Caution) => {
                        self.push_diag(
                            Hole::GfmAlert,
                            range.clone(),
                            "GFM alert mapped to `@callout(kind: caution)`".into(),
                        );
                        Container::Alert(Hole::GfmAlert, "caution")
                    }
                };
                self.out_stack.push(String::new());
                self.container_stack.push(container);
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                let inner = self.out_stack.pop().expect("unbalanced quote stack");
                let container = self
                    .container_stack
                    .pop()
                    .expect("unbalanced container stack");
                let trimmed = inner.trim_end_matches('\n');
                match container {
                    Container::Quote => {
                        for line in trimmed.split('\n') {
                            if line.is_empty() {
                                self.write_char('>');
                            } else {
                                self.write("> ");
                                self.write(line);
                            }
                            self.write_char('\n');
                        }
                    }
                    Container::Alert(_, kind) => {
                        self.write("@callout(kind: ");
                        self.write(kind);
                        self.write(")\n");
                        self.write(trimmed);
                        if !trimmed.ends_with('\n') {
                            self.write_char('\n');
                        }
                        self.write("@end\n");
                    }
                    Container::LinkPending | Container::ImagePending | Container::Paragraph => {
                        // Should not arrive here — Link/Image/Paragraph End arms
                        // pop their own buffers and containers. Defensive no-op.
                        self.write(&inner);
                    }
                }
            }
            Event::Rule => {
                let snippet = self.src.get(range.clone()).unwrap_or("").trim();
                let is_clean_dashes = snippet == "---";
                if !is_clean_dashes {
                    self.push_diag(
                        Hole::AltHorizontalRule,
                        range.clone(),
                        format!("`{}` rewritten to `---`", snippet),
                    );
                }
                self.write("---\n");
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                ..
            }) => {
                use pulldown_cmark::LinkType;
                // Capture a non-empty title to emit as `title:` kwarg.
                let link_title = if title.is_empty() {
                    None
                } else {
                    Some(title.to_string())
                };
                let mut diag: Option<(Hole, String)> = None;
                match link_type {
                    LinkType::Autolink | LinkType::Email => {
                        diag = Some((
                            Hole::AutolinkRewrap,
                            format!("autolink `<{}>` wrapped in `@link[..](..)`", dest_url),
                        ));
                    }
                    LinkType::Reference
                    | LinkType::ReferenceUnknown
                    | LinkType::Collapsed
                    | LinkType::CollapsedUnknown
                    | LinkType::Shortcut
                    | LinkType::ShortcutUnknown => {
                        diag = Some((
                            Hole::RefLinkInlined,
                            "reference-style link resolved inline".into(),
                        ));
                    }
                    LinkType::Inline => {}
                    _ => {}
                }
                // Store (url, optional_title, optional_diag) via a tuple.
                // We encode the title into the url string using a sentinel separator
                // so we can reuse the existing link_stack without changing its type.
                // Instead, push title into a separate parallel stack field by
                // storing both in a combined tuple stored in out_stack label.
                // Simplest approach: store title in a new wrapper. Use an existing
                // field trick: push url\x00title so End can split on \x00.
                let url_with_title = if let Some(ref t) = link_title {
                    format!("{}\x00{}", dest_url, t)
                } else {
                    dest_url.to_string()
                };
                self.link_stack.push((url_with_title, diag));
                self.out_stack.push(String::new());
                self.container_stack.push(Container::LinkPending);
            }
            Event::End(TagEnd::Link) => {
                let text = self.out_stack.pop().expect("link buffer");
                let _ = self.container_stack.pop();
                let (url_with_title, diag) = self.link_stack.pop().expect("link stack");
                // Split url and optional title.
                let (url, opt_title) = if let Some(idx) = url_with_title.find('\x00') {
                    let (u, t) = url_with_title.split_at(idx);
                    (u.to_string(), Some(t[1..].to_string()))
                } else {
                    (url_with_title, None)
                };
                if let Some((hole, note)) = diag {
                    self.diags.push(Diag {
                        hole,
                        line: 0,
                        col: 0,
                        original: format!("[{}]({})", text, url),
                        note,
                    });
                }
                if let Some(t) = opt_title {
                    self.write("@link(title: \"");
                    self.write(&t);
                    self.write("\")[");
                    self.write(&text);
                    self.write("](");
                    self.write(&url);
                    self.write(")");
                } else {
                    self.write("@link[");
                    self.write(&text);
                    self.write("](");
                    self.write(&url);
                    self.write(")");
                }
            }
            Event::Start(Tag::Image {
                dest_url, title, ..
            }) => {
                let diag = if title.is_empty() {
                    None
                } else {
                    Some((
                        Hole::LinkTitleDropped,
                        format!("image title `{}` dropped", title),
                    ))
                };
                self.link_stack.push((dest_url.to_string(), diag));
                self.out_stack.push(String::new());
                self.container_stack.push(Container::ImagePending);
            }
            Event::End(TagEnd::Image) => {
                let alt = self.out_stack.pop().expect("image buffer");
                let _ = self.container_stack.pop();
                let (src, diag) = self.link_stack.pop().expect("link stack");
                if let Some((hole, note)) = diag {
                    self.diags.push(Diag {
                        hole,
                        line: 0,
                        col: 0,
                        original: format!("![{}]({})", alt, src),
                        note,
                    });
                }
                self.write("@image(src: \"");
                self.write(&src);
                self.write("\", alt: \"");
                self.write(&alt);
                self.write("\")[]");
            }
            Event::Start(Tag::Table(aligns)) => {
                self.table = Some(TableState {
                    aligns,
                    rows: Vec::new(),
                    current_row: Vec::new(),
                    current_cell: String::new(),
                    in_cell: false,
                });
            }
            Event::End(TagEnd::Table) => {
                if let Some(state) = self.table.take() {
                    self.emit_table(state);
                }
            }
            Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => {
                if let Some(t) = self.table.as_mut() {
                    t.current_row = Vec::new();
                }
            }
            Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) => {
                if let Some(t) = self.table.as_mut() {
                    let row = std::mem::take(&mut t.current_row);
                    t.rows.push(row);
                }
            }
            Event::Start(Tag::TableCell) => {
                if let Some(t) = self.table.as_mut() {
                    t.current_cell = String::new();
                    t.in_cell = true;
                }
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(t) = self.table.as_mut() {
                    let cell = std::mem::take(&mut t.current_cell);
                    t.current_row.push(cell);
                    t.in_cell = false;
                }
            }
            Event::Start(Tag::MetadataBlock(kind)) => {
                self.in_metadata = true;
                self.metadata_buf.clear();
                self.metadata_kind = Some(kind);
            }
            Event::End(TagEnd::MetadataBlock(_)) => {
                use pulldown_cmark::MetadataBlockKind;
                self.in_metadata = false;
                let kind = self.metadata_kind.take();
                let body = std::mem::take(&mut self.metadata_buf);
                match kind {
                    Some(MetadataBlockKind::PlusesStyle) => {
                        // TOML markdown frontmatter has a clean Brief equivalent;
                        // emit it directly with no hole diagnostic.
                        self.write("+++\n");
                        self.write(&body);
                        if !body.ends_with('\n') {
                            self.write_char('\n');
                        }
                        self.write("+++\n\n");
                    }
                    _ => {
                        self.push_diag(
                            Hole::Frontmatter,
                            range.clone(),
                            "frontmatter dropped, replaced with TODO comment".into(),
                        );
                        let summary: String =
                            body.chars().take(60).collect::<String>().replace('\n', " ");
                        self.write("// TODO[B-hole:frontmatter]: ");
                        self.write(&summary);
                        self.write_char('\n');
                    }
                }
            }
            Event::Start(Tag::HtmlBlock) => {
                self.push_diag(
                    Hole::HtmlBlock,
                    range.clone(),
                    "HTML block preserved inside Brief block comment".into(),
                );
                self.write("// TODO[B-hole:html-block]\n");
                self.write("/*\n");
            }
            Event::End(TagEnd::HtmlBlock) => {
                self.write("\n*/\n");
            }
            Event::Html(s) => {
                // Block-level HTML content (between Start/End of HtmlBlock).
                self.write(&s);
            }
            Event::InlineHtml(s) => {
                let snippet = s.to_string();
                self.push_diag(
                    Hole::InlineHtml,
                    range.clone(),
                    format!("`{}` preserved as TODO comment", snippet.trim()),
                );
                self.pending_html_comments
                    .push(format!("// TODO[B-hole:inline-html]: {}", snippet.trim()));
            }
            Event::FootnoteReference(label) => {
                let body = self
                    .footnote_defs
                    .get(label.as_ref())
                    .cloned()
                    .unwrap_or_else(|| format!("??: {}", label));
                self.write("@footnote[");
                self.write(&body);
                self.write("]");
            }
            Event::SoftBreak => {
                self.write_char('\n');
            }
            Event::HardBreak => {
                self.write_char('\\');
                self.write_char('\n');
            }
            Event::InlineMath(s) => {
                self.write("@math[");
                self.write(&s);
                self.write_char(']');
            }
            Event::DisplayMath(s) => {
                let body = s.trim_matches('\n');
                self.write("@math\n");
                self.write(body);
                self.write_char('\n');
                self.write("@end");
            }
            _ => {
                // Other events handled in subsequent tasks.
            }
        }
    }

    fn finish(mut self) -> ConvertResult {
        // Trim trailing blank lines down to a single newline.
        while self.out.ends_with("\n\n") {
            self.out.pop();
        }
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push('\n');
        }
        ConvertResult {
            brief_source: self.out,
            diagnostics: self.diags,
        }
    }

    /// Convert a byte offset into 1-indexed (line, column).
    #[allow(dead_code)] // used by later tasks
    fn pos(&self, offset: usize) -> (usize, usize) {
        match self.line_offsets.binary_search(&offset) {
            Ok(line) => (line + 1, 1),
            Err(line) => {
                let line_start = self.line_offsets[line - 1];
                (line, offset - line_start + 1)
            }
        }
    }

    #[allow(dead_code)] // used by later tasks
    fn push_diag(&mut self, hole: Hole, range: std::ops::Range<usize>, note: String) {
        let (line, col) = self.pos(range.start);
        let original = self
            .src
            .get(range.clone())
            .unwrap_or("")
            .chars()
            .take(80)
            .collect::<String>();
        self.diags.push(Diag {
            hole,
            line,
            col,
            original,
            note,
        });
    }

    fn emit_table(&mut self, state: TableState) {
        use pulldown_cmark::Alignment;
        let needs_align = state.aligns.iter().any(|a| !matches!(a, Alignment::None));
        if needs_align {
            self.write("@t(align: [");
            let parts: Vec<&str> = state
                .aligns
                .iter()
                .map(|a| match a {
                    Alignment::None | Alignment::Left => "left",
                    Alignment::Center => "center",
                    Alignment::Right => "right",
                })
                .collect();
            self.write(&parts.join(", "));
            self.write("])\n");
        } else {
            self.write("@t\n");
        }
        for row in &state.rows {
            self.write("|");
            for (i, cell) in row.iter().enumerate() {
                self.write(" ");
                self.write(cell.trim());
                if i + 1 < row.len() {
                    self.write(" |");
                }
            }
            self.write("\n");
        }
    }
}

fn compute_line_offsets(s: &str) -> Vec<usize> {
    let mut v = vec![0usize];
    for (i, b) in s.bytes().enumerate() {
        if b == b'\n' {
            v.push(i + 1);
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_toml_frontmatter_cleanly() {
        let md = "+++\ntitle = \"hi\"\nn = 3\n+++\n\n# Doc\nbody\n";
        let res = convert(md, "in.md");
        assert!(
            !res.diagnostics.iter().any(|d| d.hole == Hole::Frontmatter),
            "{:?}",
            res.diagnostics
        );
        assert!(
            res.brief_source.starts_with("+++\n"),
            "got: {}",
            res.brief_source
        );
        assert!(res.brief_source.contains("title = \"hi\""));
        assert!(res.brief_source.contains("n = 3"));
        assert!(res.brief_source.contains("\n+++\n"));
        assert!(res.brief_source.contains("# Doc"));
    }

    #[test]
    fn converts_yaml_frontmatter_as_hole() {
        let md = "---\ntitle: hi\n---\n\n# Doc\n";
        let res = convert(md, "in.md");
        assert!(
            res.diagnostics.iter().any(|d| d.hole == Hole::Frontmatter),
            "{:?}",
            res.diagnostics
        );
        assert!(
            res.brief_source.contains("// TODO[B-hole:frontmatter]"),
            "{}",
            res.brief_source
        );
    }

    #[test]
    fn converted_toml_frontmatter_round_trips_through_compiler() {
        let md = "+++\ntitle = \"hi\"\n+++\n\n# Doc\n";
        let res = convert(md, "in.md");
        let src = crate::span::SourceMap::new("in.brf", res.brief_source.clone());
        let toks = crate::lexer::lex(&src).expect("lex ok");
        let (doc, diags) = crate::parser::parse(toks, &src);
        assert!(diags.is_empty(), "{:?}\n---\n{}", diags, res.brief_source);
        assert!(doc.metadata.is_some());
    }
}
