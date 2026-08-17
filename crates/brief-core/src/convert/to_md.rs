//! Brief-to-Markdown converter.
//!
//! Walks the parsed `Document` and emits GFM-flavoured Markdown. Every Brief
//! construct that has no clean Markdown equivalent is reported as an `MdDiag`
//! carrying an `MdHole` code; nothing is silently dropped. Where GFM offers a
//! syntax the Markdown→Brief converter already reads (TOML `+++` frontmatter,
//! `{#anchor}` heading attributes, `> [!NOTE]` alerts, `term`/`: definition`
//! lists), that syntax is chosen so a round-trip re-converts cleanly.

use crate::ast::{Block, CodeAttrs, Document, Inline, ShortArgs, TaskState};
use crate::emit::html::{escape_html_into, expand_template};
use crate::shortcode::{ArgValue, Registry};
use crate::span::{SourceMap, Span};
use std::fmt::Write;

#[derive(Clone, Debug)]
pub struct MdResult {
    pub markdown: String,
    pub diagnostics: Vec<MdDiag>,
}

#[derive(Clone, Debug)]
pub struct MdDiag {
    pub hole: MdHole,
    pub line: usize, // 1-indexed
    pub col: usize,  // 1-indexed
    pub note: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MdHole {
    Underline,
    Comment,
    CustomShortcode,
    CodeAttrs,
    RefUnresolved,
}

impl MdHole {
    /// Stable kebab-case slug used in stderr report lines.
    pub fn slug(self) -> &'static str {
        match self {
            MdHole::Underline => "underline-html",
            MdHole::Comment => "comment-dropped",
            MdHole::CustomShortcode => "custom-shortcode",
            MdHole::CodeAttrs => "code-attrs-dropped",
            MdHole::RefUnresolved => "ref-unresolved",
        }
    }

    /// Short human-readable label for stderr.
    pub fn message(self) -> &'static str {
        match self {
            MdHole::Underline => "Brief underline `+x+` emitted as inline HTML `<u>`",
            MdHole::Comment => "comment dropped (Markdown has no comment syntax)",
            MdHole::CustomShortcode => {
                "custom shortcode has no Markdown form; expanded via its HTML template or unwrapped"
            }
            MdHole::CodeAttrs => "code fence attribute (`@nominify`/`@minify`/...) dropped",
            MdHole::RefUnresolved => "`@ref` target could not be parsed; emitted as plain text",
        }
    }
}

/// Convert a parsed (and ideally resolved) Brief document to Markdown.
///
/// `src` is the SourceMap the document was parsed from; it is used to locate
/// comments (which the parser strips before the AST is built) and to map
/// spans to line/column pairs for diagnostics. Run the resolver first so
/// positional shortcode arguments are bound to their keyword slots.
pub fn to_markdown(doc: &Document, reg: &Registry, src: &SourceMap) -> MdResult {
    let mut ctx = Ctx {
        reg,
        src,
        resolved_refs: &doc.resolved_refs,
        diags: Vec::new(),
        footnotes: Vec::new(),
        in_footnote: false,
    };
    let mut out = String::new();
    if let Some(meta) = &doc.metadata {
        let body = toml::to_string(meta).unwrap_or_default();
        out.push_str("+++\n");
        out.push_str(&body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("+++\n\n");
    }
    let body = render_blocks(&doc.blocks, &mut ctx);
    out.push_str(&body);
    if !ctx.footnotes.is_empty() {
        out.push('\n');
        for (i, b) in ctx.footnotes.iter().enumerate() {
            let _ = writeln!(out, "[^{}]: {}", i + 1, b);
        }
    }
    scan_comments(src, &mut ctx.diags);
    MdResult {
        markdown: normalize(&out),
        diagnostics: ctx.diags,
    }
}

struct Ctx<'a> {
    reg: &'a Registry,
    src: &'a SourceMap,
    resolved_refs: &'a std::collections::BTreeMap<Span, crate::ast::ResolvedRef>,
    diags: Vec<MdDiag>,
    /// Rendered footnote bodies in reference order; emitted as `[^n]:`
    /// definitions at the end of the document.
    footnotes: Vec<String>,
    in_footnote: bool,
}

impl<'a> Ctx<'a> {
    fn hole(&mut self, hole: MdHole, span: Span, note: impl Into<String>) {
        let (line, col) = self.src.line_col(span.start);
        self.diags.push(MdDiag {
            hole,
            line,
            col,
            note: note.into(),
        });
    }
}

/// Render a block sequence; blocks are separated by a single blank line.
/// Each rendered block ends with `\n`, so joining on `\n` yields the blank.
fn render_blocks(blocks: &[Block], ctx: &mut Ctx) -> String {
    let mut parts: Vec<String> = Vec::new();
    for b in blocks {
        let mut s = String::new();
        render_block(b, ctx, &mut s);
        if !s.is_empty() {
            parts.push(s);
        }
    }
    parts.join("\n")
}

fn render_block(b: &Block, ctx: &mut Ctx, out: &mut String) {
    match b {
        Block::Heading {
            level,
            content,
            anchor,
            ..
        } => {
            for _ in 0..*level {
                out.push('#');
            }
            out.push(' ');
            render_inline_seq(content, ctx, out, false);
            if let Some(a) = anchor {
                let _ = write!(out, " {{#{}}}", a);
            }
            out.push('\n');
        }
        Block::Paragraph { content, .. } => {
            if content.is_empty() {
                return;
            }
            render_inline_seq(content, ctx, out, false);
            out.push('\n');
        }
        Block::List { ordered, items, .. } => {
            for (i, it) in items.iter().enumerate() {
                let marker = if *ordered {
                    format!("{}. ", i + 1)
                } else {
                    "- ".to_string()
                };
                out.push_str(&marker);
                if let Some(state) = it.task {
                    out.push_str(match state {
                        TaskState::Done => "[x] ",
                        TaskState::Todo => "[ ] ",
                    });
                }
                render_inline_seq(&it.content, ctx, out, false);
                out.push('\n');
                let indent = " ".repeat(marker.len());
                for c in &it.children {
                    let mut s = String::new();
                    render_block(c, ctx, &mut s);
                    if s.is_empty() {
                        continue;
                    }
                    // A nested list attaches directly under the item text;
                    // any other child block needs a blank line so Markdown
                    // does not lazy-continue it into the item's paragraph.
                    if !matches!(c, Block::List { .. }) {
                        out.push('\n');
                    }
                    for line in s.lines() {
                        if line.is_empty() {
                            out.push('\n');
                        } else {
                            out.push_str(&indent);
                            out.push_str(line);
                            out.push('\n');
                        }
                    }
                }
            }
        }
        Block::Blockquote { children, .. } => {
            let inner = render_blocks(children, ctx);
            quote_prefix(&inner, out);
        }
        Block::CodeBlock {
            lang,
            body,
            attrs,
            span,
        } => {
            if *attrs != CodeAttrs::default() {
                ctx.hole(
                    MdHole::CodeAttrs,
                    *span,
                    "code fence attribute dropped (no Markdown equivalent)",
                );
            }
            let fence = "`".repeat(fence_len(body));
            out.push_str(&fence);
            if let Some(l) = lang {
                out.push_str(l);
            }
            out.push('\n');
            out.push_str(body);
            if !body.is_empty() && !body.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&fence);
            out.push('\n');
        }
        Block::Table {
            args, header, rows, ..
        } => {
            let aligns: Vec<&str> = match args.keyword.get("align") {
                Some(ArgValue::Array(a)) => a.iter().filter_map(|v| v.as_str()).collect(),
                _ => Vec::new(),
            };
            render_table_row(&header.cells, ctx, out);
            out.push('|');
            for i in 0..header.cells.len() {
                let rule = match aligns.get(i).copied() {
                    Some("left") => ":---",
                    Some("center") => ":---:",
                    Some("right") => "---:",
                    _ => "---",
                };
                let _ = write!(out, " {} |", rule);
            }
            out.push('\n');
            for row in rows {
                render_table_row(&row.cells, ctx, out);
            }
        }
        Block::DefinitionList { items, .. } => {
            let mut parts: Vec<String> = Vec::new();
            for it in items {
                let mut s = String::new();
                render_inline_seq(&it.term, ctx, &mut s, false);
                s.push('\n');
                s.push_str(": ");
                render_inline_seq(&it.definition, ctx, &mut s, false);
                s.push('\n');
                parts.push(s);
            }
            out.push_str(&parts.join("\n"));
        }
        Block::HorizontalRule { .. } => out.push_str("---\n"),
        Block::BlockShortcode {
            name,
            args,
            children,
            span,
        } => render_block_shortcode(name, args, children, *span, ctx, out),
    }
}

fn render_block_shortcode(
    name: &str,
    args: &ShortArgs,
    children: &[Block],
    span: Span,
    ctx: &mut Ctx,
    out: &mut String,
) {
    if let Some(sc) = ctx.reg.get(name) {
        if let Some(t) = &sc.template_html {
            let inner = render_blocks(children, ctx);
            out.push_str(&expand_template(t, args, inner.trim_end()));
            if !out.ends_with('\n') {
                out.push('\n');
            }
            ctx.hole(
                MdHole::CustomShortcode,
                span,
                format!("`@{}` expanded via its `template_html`", name),
            );
            return;
        }
    }
    match name {
        "callout" => {
            let kind = args
                .keyword
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("note");
            let _ = writeln!(out, "> [!{}]", kind.to_ascii_uppercase());
            let inner = render_blocks(children, ctx);
            quote_prefix(&inner, out);
        }
        "details" => {
            let summary = args
                .keyword
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            out.push_str("<details><summary>");
            escape_html_into(out, summary);
            out.push_str("</summary>\n\n");
            let inner = render_blocks(children, ctx);
            out.push_str(&inner);
            out.push_str("\n</details>\n");
        }
        "math" => {
            let mut body = String::new();
            for c in children {
                if let Block::Paragraph { content, .. } = c {
                    render_inline_seq(content, ctx, &mut body, true);
                    body.push('\n');
                } else {
                    render_block(c, ctx, &mut body);
                }
            }
            out.push_str("$$\n");
            out.push_str(body.trim_end());
            out.push_str("\n$$\n");
        }
        _ => {
            let inner = render_blocks(children, ctx);
            out.push_str(&inner);
            ctx.hole(
                MdHole::CustomShortcode,
                span,
                format!(
                    "`@{}` has no Markdown form; children emitted without the wrapper",
                    name
                ),
            );
        }
    }
}

fn render_table_row(cells: &[Vec<Inline>], ctx: &mut Ctx, out: &mut String) {
    out.push('|');
    for cell in cells {
        let mut s = String::new();
        render_inline_seq(cell, ctx, &mut s, false);
        let _ = write!(out, " {} |", s.replace('|', "\\|"));
    }
    out.push('\n');
}

/// Prefix every line of `inner` with `> ` (bare `>` on blank lines).
fn quote_prefix(inner: &str, out: &mut String) {
    for line in inner.lines() {
        if line.is_empty() {
            out.push_str(">\n");
        } else {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
    }
}

fn render_inline_seq(seq: &[Inline], ctx: &mut Ctx, out: &mut String, raw: bool) {
    let mut after_break = false;
    for n in seq {
        // The parser joins continuation lines with a space, so the Text
        // that follows a HardBreak carries a leading space; Markdown would
        // render it as indentation, so trim it.
        if after_break {
            if let Inline::Text { value, .. } = n {
                render_text(value.trim_start(), out, raw);
                after_break = false;
                continue;
            }
        }
        after_break = matches!(n, Inline::HardBreak { .. });
        render_inline(n, ctx, out, raw);
    }
}

fn render_inline(node: &Inline, ctx: &mut Ctx, out: &mut String, raw: bool) {
    match node {
        Inline::Text { value, .. } => render_text(value, out, raw),
        Inline::HardBreak { .. } => out.push_str("\\\n"),
        Inline::Bold { content, .. } => {
            out.push_str("**");
            render_inline_seq(content, ctx, out, raw);
            out.push_str("**");
        }
        Inline::Italic { content, .. } => {
            out.push('_');
            render_inline_seq(content, ctx, out, raw);
            out.push('_');
        }
        Inline::Strike { content, .. } => {
            out.push_str("~~");
            render_inline_seq(content, ctx, out, raw);
            out.push_str("~~");
        }
        Inline::Underline { content, span } => {
            out.push_str("<u>");
            render_inline_seq(content, ctx, out, raw);
            out.push_str("</u>");
            ctx.hole(
                MdHole::Underline,
                *span,
                "underline has no Markdown syntax; emitted as `<u>`",
            );
        }
        Inline::InlineCode { value, .. } => code_span(value, out),
        Inline::Shortcode {
            name,
            args,
            content,
            span,
        } => render_inline_shortcode(name, args, content.as_deref(), *span, ctx, out, raw),
    }
}

fn render_inline_shortcode(
    name: &str,
    args: &ShortArgs,
    content: Option<&[Inline]>,
    span: Span,
    ctx: &mut Ctx,
    out: &mut String,
    raw: bool,
) {
    if let Some(sc) = ctx.reg.get(name) {
        if let Some(t) = &sc.template_html {
            let inner = content
                .map(|c| {
                    let mut s = String::new();
                    render_inline_seq(c, ctx, &mut s, raw);
                    s
                })
                .unwrap_or_default();
            out.push_str(&expand_template(t, args, &inner));
            ctx.hole(
                MdHole::CustomShortcode,
                span,
                format!("`@{}` expanded via its `template_html`", name),
            );
            return;
        }
    }
    match name {
        "link" => {
            let url = args
                .keyword
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            out.push('[');
            if let Some(c) = content {
                render_inline_seq(c, ctx, out, raw);
            }
            out.push_str("](");
            out.push_str(url);
            if let Some(t) = args.keyword.get("title").and_then(|v| v.as_str()) {
                let _ = write!(out, " \"{}\"", t.replace('"', "\\\""));
            }
            out.push(')');
        }
        "image" => {
            let alt = args
                .keyword
                .get("alt")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let src = args
                .keyword
                .get("src")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            out.push_str("![");
            render_text(alt, out, false);
            out.push_str("](");
            out.push_str(src);
            out.push(')');
        }
        "kbd" | "sub" | "sup" => {
            let _ = write!(out, "<{}>", name);
            if let Some(c) = content {
                render_inline_seq(c, ctx, out, raw);
            }
            let _ = write!(out, "</{}>", name);
        }
        "math" => {
            out.push('$');
            if let Some(c) = content {
                render_inline_seq(c, ctx, out, true);
            }
            out.push('$');
        }
        "footnote" => {
            let Some(c) = content else { return };
            if ctx.in_footnote {
                // Keep document-level numbering linear: a footnote nested
                // inside another footnote's body degrades to bracketed text.
                out.push('[');
                render_inline_seq(c, ctx, out, raw);
                out.push(']');
                return;
            }
            ctx.in_footnote = true;
            let mut body = String::new();
            render_inline_seq(c, ctx, &mut body, false);
            ctx.in_footnote = false;
            ctx.footnotes.push(body);
            let _ = write!(out, "[^{}]", ctx.footnotes.len());
        }
        "ref" => {
            let display = ctx
                .resolved_refs
                .get(&span)
                .map(|r| r.display.clone())
                .or_else(|| {
                    args.keyword
                        .get("title")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
                .or_else(|| {
                    args.positional
                        .first()
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
                .unwrap_or_default();
            // Use the target as the author wrote it (relative to this file)
            // rather than the project-relative resolved path, so links keep
            // working when a whole tree is converted in place.
            let target: String = content
                .map(|c| {
                    let mut s = String::new();
                    for n in c {
                        if let Inline::Text { value, .. } = n {
                            s.push_str(value);
                        }
                    }
                    s
                })
                .unwrap_or_default();
            let target = target.trim();
            let (path, anchor) = match target.split_once('#') {
                Some((p, a)) => (p, Some(a)),
                None => (target, None),
            };
            match path.strip_suffix(".brf") {
                Some(stem) if !stem.is_empty() => {
                    out.push('[');
                    render_text(&display, out, false);
                    out.push_str("](");
                    out.push_str(stem);
                    out.push_str(".md");
                    if let Some(a) = anchor {
                        out.push('#');
                        out.push_str(a);
                    }
                    out.push(')');
                }
                _ => {
                    render_text(&display, out, false);
                    ctx.hole(
                        MdHole::RefUnresolved,
                        span,
                        format!(
                            "`@ref` target `{}` could not be rewritten to a `.md` link",
                            target
                        ),
                    );
                }
            }
        }
        _ => {
            if let Some(c) = content {
                render_inline_seq(c, ctx, out, raw);
            }
            ctx.hole(
                MdHole::CustomShortcode,
                span,
                format!(
                    "`@{}` has no Markdown form; content emitted without the wrapper",
                    name
                ),
            );
        }
    }
}

/// Escape Markdown-significant punctuation in a text run. Brief's stricter
/// line grammar means paragraph lines can never begin with a block sigil
/// (`#`, `>`, `- `, `1. `), so only inline-level characters need escaping.
fn render_text(s: &str, out: &mut String, raw: bool) {
    if raw {
        out.push_str(s);
        return;
    }
    for ch in s.chars() {
        if matches!(ch, '\\' | '`' | '*' | '_' | '~' | '[' | ']' | '<' | '&') {
            out.push('\\');
        }
        out.push(ch);
    }
}

/// Emit an inline code span, widening the backtick delimiter past the
/// longest run inside the value and padding when the value starts or ends
/// with a backtick.
fn code_span(value: &str, out: &mut String) {
    let mut max_run = 0usize;
    let mut run = 0usize;
    for ch in value.chars() {
        if ch == '`' {
            run += 1;
            max_run = max_run.max(run);
        } else {
            run = 0;
        }
    }
    let delim = "`".repeat(max_run + 1);
    let pad = value.starts_with('`') || value.ends_with('`');
    out.push_str(&delim);
    if pad {
        out.push(' ');
    }
    out.push_str(value);
    if pad {
        out.push(' ');
    }
    out.push_str(&delim);
}

/// Fence length for a code block body: at least 3 backticks, and one more
/// than the longest backtick run inside the body.
fn fence_len(body: &str) -> usize {
    let mut max_run = 0usize;
    let mut run = 0usize;
    for ch in body.chars() {
        if ch == '`' {
            run += 1;
            max_run = max_run.max(run);
        } else {
            run = 0;
        }
    }
    (max_run + 1).max(3)
}

/// The parser strips `//` and `/* */` comments before the AST is built, so
/// the emitter never sees them. Re-scan the source with the parser's own
/// line rules (comments exist only at block position, never inside code
/// fences or frontmatter) and report each one as dropped.
fn scan_comments(src: &SourceMap, diags: &mut Vec<MdDiag>) {
    let mut in_fence = false;
    let mut in_block_comment = false;
    let mut in_frontmatter = false;
    for (i, line) in src.source.lines().enumerate() {
        let lineno = i + 1;
        if i == 0 && line.trim_end() == "+++" {
            in_frontmatter = true;
            continue;
        }
        if in_frontmatter {
            if line.trim_end() == "+++" {
                in_frontmatter = false;
            }
            continue;
        }
        if in_block_comment {
            if line.trim_end().ends_with("*/") {
                in_block_comment = false;
            }
            continue;
        }
        let t = line.trim_start();
        if in_fence {
            if t.starts_with("```") {
                in_fence = false;
            }
            continue;
        }
        if t.starts_with("```") {
            in_fence = true;
            continue;
        }
        let col = line.len() - t.len() + 1;
        if t.starts_with("//") {
            diags.push(MdDiag {
                hole: MdHole::Comment,
                line: lineno,
                col,
                note: "`//` comment dropped".into(),
            });
            continue;
        }
        if t.starts_with("/*") {
            diags.push(MdDiag {
                hole: MdHole::Comment,
                line: lineno,
                col,
                note: "`/* */` comment dropped".into(),
            });
            let te = t.trim_end();
            if !(te.ends_with("*/") && te.len() >= 4) {
                in_block_comment = true;
            }
        }
    }
}

/// Collapse runs of 3+ newlines to 2, drop leading blank lines, and end
/// with exactly one trailing newline.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut nl_run = 0usize;
    for ch in s.chars() {
        if ch == '\n' {
            nl_run += 1;
            if nl_run <= 2 {
                out.push('\n');
            }
        } else {
            nl_run = 0;
            out.push(ch);
        }
    }
    let trimmed = out.trim_start_matches('\n').trim_end_matches('\n');
    if trimmed.is_empty() {
        return String::new();
    }
    format!("{}\n", trimmed)
}
