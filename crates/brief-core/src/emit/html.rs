use crate::ast::{Block, Document, Inline, ListItem, Row, ShortArgs};
use crate::shortcode::{ArgValue, Registry};
use std::fmt::Write;

pub fn render(doc: &Document, reg: &Registry) -> String {
    let footnotes = collect_footnotes(doc);
    let mut ctx = Ctx {
        reg,
        counter: 0,
        in_footnote: false,
        resolved_refs: &doc.resolved_refs,
    };
    let mut out = String::new();
    for b in &doc.blocks {
        render_block(b, &mut ctx, &mut out);
    }
    if !footnotes.is_empty() {
        emit_footnotes_section(&footnotes, reg, &doc.resolved_refs, &mut out);
    }
    out
}

struct Ctx<'a> {
    reg: &'a Registry,
    counter: u32,
    in_footnote: bool,
    resolved_refs: &'a std::collections::BTreeMap<crate::span::Span, crate::ast::ResolvedRef>,
}

fn render_block(block: &Block, ctx: &mut Ctx, out: &mut String) {
    match block {
        Block::Heading {
            level,
            content,
            anchor,
            ..
        } => {
            if let Some(a) = anchor {
                let _ = write!(out, "<h{} id=\"{}\">", level, escape_attr(a));
            } else {
                let _ = write!(out, "<h{}>", level);
            }
            render_inline_seq(content, ctx, out);
            let _ = writeln!(out, "</h{}>", level);
        }
        Block::Paragraph { content, .. } => {
            if content.is_empty() {
                return;
            }
            out.push_str("<p>");
            render_inline_seq(content, ctx, out);
            out.push_str("</p>\n");
        }
        Block::List { ordered, items, .. } => {
            let tag = if *ordered { "ol" } else { "ul" };
            let has_tasks = items.iter().any(|it| it.task.is_some());
            if has_tasks {
                let _ = writeln!(out, "<{} class=\"contains-task-list\">", tag);
            } else {
                let _ = writeln!(out, "<{}>", tag);
            }
            for it in items {
                render_item(it, ctx, out);
            }
            let _ = writeln!(out, "</{}>", tag);
        }
        Block::Blockquote { children, .. } => {
            out.push_str("<blockquote>\n");
            for c in children {
                render_block(c, ctx, out);
            }
            out.push_str("</blockquote>\n");
        }
        Block::CodeBlock { lang, body, .. } => {
            // HTML emit ignores `attrs` — minification is exclusively an
            // LLM-mode concern (per design §3 and §7.4).
            match lang {
                Some(l) => {
                    let _ = write!(out, "<pre><code class=\"language-{}\">", escape_attr(l));
                }
                None => out.push_str("<pre><code>"),
            }
            escape_html_into(out, body);
            out.push_str("</code></pre>\n");
        }
        Block::Table {
            args, header, rows, ..
        } => {
            render_table(args, header, rows, ctx, out);
        }
        Block::DefinitionList { items, .. } => {
            out.push_str("<dl>\n");
            for it in items {
                out.push_str("<dt>");
                render_inline_seq(&it.term, ctx, out);
                out.push_str("</dt>\n<dd>");
                render_inline_seq(&it.definition, ctx, out);
                out.push_str("</dd>\n");
            }
            out.push_str("</dl>\n");
        }
        Block::HorizontalRule { .. } => out.push_str("<hr>\n"),
        Block::BlockShortcode {
            name,
            args,
            children,
            ..
        } => {
            let inner = {
                let mut s = String::new();
                for c in children {
                    render_block(c, ctx, &mut s);
                }
                s
            };
            render_shortcode_html(name, args, Some(&inner), ctx, out);
        }
    }
}

fn render_item(it: &ListItem, ctx: &mut Ctx, out: &mut String) {
    use crate::ast::TaskState;
    match it.task {
        None => out.push_str("<li>"),
        Some(state) => {
            let checked = matches!(state, TaskState::Done);
            let _ = write!(
                out,
                "<li class=\"task-list-item\"><input type=\"checkbox\" disabled{}> ",
                if checked { " checked" } else { "" }
            );
        }
    }
    render_inline_seq(&it.content, ctx, out);
    if !it.children.is_empty() {
        out.push('\n');
        for c in &it.children {
            render_block(c, ctx, out);
        }
    }
    out.push_str("</li>\n");
}

fn render_table(args: &ShortArgs, header: &Row, rows: &[Row], ctx: &mut Ctx, out: &mut String) {
    // The parser rejects align values outside this set, but the renderer
    // must stay safe on any AST it's handed: these strings land in a style
    // attribute, so only whitelisted values may pass.
    let aligns: Vec<&str> = if let Some(ArgValue::Array(a)) = args.keyword.get("align") {
        a.iter()
            .map(|v| match v.as_str() {
                Some(s @ ("left" | "right" | "center")) => s,
                _ => "left",
            })
            .collect()
    } else {
        vec!["left"; header.cells.len()]
    };
    out.push_str("<table>\n<thead><tr>");
    for (i, c) in header.cells.iter().enumerate() {
        let a = aligns.get(i).copied().unwrap_or("left");
        let _ = write!(out, "<th style=\"text-align:{}\">", a);
        render_inline_seq(c, ctx, out);
        out.push_str("</th>");
    }
    out.push_str("</tr></thead>\n<tbody>\n");
    for r in rows {
        out.push_str("<tr>");
        for (i, c) in r.cells.iter().enumerate() {
            let a = aligns.get(i).copied().unwrap_or("left");
            let _ = write!(out, "<td style=\"text-align:{}\">", a);
            render_inline_seq(c, ctx, out);
            out.push_str("</td>");
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</tbody>\n</table>\n");
}

fn render_inline_seq(seq: &[Inline], ctx: &mut Ctx, out: &mut String) {
    for n in seq {
        render_inline(n, ctx, out);
    }
}

fn render_inline(node: &Inline, ctx: &mut Ctx, out: &mut String) {
    match node {
        Inline::Text { value, .. } => escape_html_into(out, value),
        Inline::HardBreak { .. } => out.push_str("<br>"),
        Inline::Bold { content, .. } => {
            out.push_str("<strong>");
            render_inline_seq(content, ctx, out);
            out.push_str("</strong>");
        }
        Inline::Italic { content, .. } => {
            out.push_str("<em>");
            render_inline_seq(content, ctx, out);
            out.push_str("</em>");
        }
        Inline::Underline { content, .. } => {
            out.push_str("<u>");
            render_inline_seq(content, ctx, out);
            out.push_str("</u>");
        }
        Inline::Strike { content, .. } => {
            out.push_str("<s>");
            render_inline_seq(content, ctx, out);
            out.push_str("</s>");
        }
        Inline::InlineCode { value, .. } => {
            out.push_str("<code>");
            escape_html_into(out, value);
            out.push_str("</code>");
        }
        Inline::Shortcode {
            name,
            args,
            content: _,
            span,
            ..
        } if name == "ref" => {
            // The display text for @ref lives in args["title"] after resolve
            // (bind_positional moves positional arg 1 → keyword["title"]).
            // Before resolve (third-test scenario), it stays in positional[0].
            let title_kw = args.keyword.get("title").and_then(|v| v.as_str());
            let title_pos = args.positional.first().and_then(|v| v.as_str());
            let resolved = ctx.resolved_refs.get(span);
            let display_text = resolved
                .map(|r| r.display.as_str())
                .or(title_kw)
                .or(title_pos)
                .unwrap_or("");
            if let Some(r) = resolved {
                let mut url = r.target_path.clone();
                debug_assert!(
                    url.ends_with(".brf"),
                    "ResolvedRef.target_path must end in .brf"
                );
                url.replace_range(url.len() - 4.., ".html");
                if let Some(a) = &r.target_anchor {
                    url.push('#');
                    url.push_str(a);
                }
                let _ = write!(
                    out,
                    "<a href=\"{}\">{}</a>",
                    escape_attr(&url),
                    escape_html(display_text)
                );
            } else {
                // No project context — emit display text only, no anchor.
                let _ = write!(out, "{}", escape_html(display_text));
            }
        }
        Inline::Shortcode {
            name,
            args,
            content,
            ..
        } => {
            // Footnote refs are auto-numbered during the main render pass.
            // Inside a footnote body we degrade nested footnotes to plain
            // bracketed text so they don't disturb the document-level
            // numbering scheme.
            if name == "footnote" {
                if content.is_none() {
                    return;
                }
                if ctx.in_footnote {
                    out.push('[');
                    if let Some(c) = content {
                        render_inline_seq(c, ctx, out);
                    }
                    out.push(']');
                    return;
                }
                ctx.counter += 1;
                let n = ctx.counter;
                let _ = write!(
                    out,
                    "<sup class=\"fn-ref\"><a id=\"fn-ref-{}\" href=\"#fn-{}\">{}</a></sup>",
                    n, n, n
                );
                return;
            }
            let inner = content.as_ref().map(|c| {
                let mut s = String::new();
                render_inline_seq(c, ctx, &mut s);
                s
            });
            render_shortcode_html(name, args, inner.as_deref(), ctx, out);
        }
    }
}

fn render_shortcode_html(
    name: &str,
    args: &ShortArgs,
    inner: Option<&str>,
    ctx: &mut Ctx,
    out: &mut String,
) {
    if let Some(sc) = ctx.reg.get(name) {
        if let Some(t) = &sc.template_html {
            let r = expand_template(t, args, inner.unwrap_or(""));
            out.push_str(&r);
            return;
        }
    }
    match name {
        "link" => {
            let url = args
                .keyword
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("#");
            let title = args.keyword.get("title").and_then(|v| v.as_str());
            if let Some(t) = title {
                let _ = write!(
                    out,
                    "<a href=\"{}\" title=\"{}\">{}</a>",
                    escape_attr(url),
                    escape_attr(t),
                    inner.unwrap_or("")
                );
            } else {
                let _ = write!(
                    out,
                    "<a href=\"{}\">{}</a>",
                    escape_attr(url),
                    inner.unwrap_or("")
                );
            }
        }
        "image" => {
            let src = args
                .keyword
                .get("src")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let alt = args
                .keyword
                .get("alt")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let _ = write!(
                out,
                "<img src=\"{}\" alt=\"{}\">",
                escape_attr(src),
                escape_attr(alt)
            );
        }
        "kbd" => {
            let _ = write!(out, "<kbd>{}</kbd>", inner.unwrap_or(""));
        }
        "sub" => {
            let _ = write!(out, "<sub>{}</sub>", inner.unwrap_or(""));
        }
        "sup" => {
            let _ = write!(out, "<sup>{}</sup>", inner.unwrap_or(""));
        }
        "details" => {
            let summary = args
                .keyword
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let _ = write!(
                out,
                "<details><summary>{}</summary>{}</details>\n",
                escape_html(summary),
                inner.unwrap_or("")
            );
        }
        "callout" => {
            let kind = args
                .keyword
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("info");
            let _ = write!(
                out,
                "<aside class=\"callout callout-{}\">{}</aside>\n",
                escape_attr(kind),
                inner.unwrap_or("")
            );
        }
        "math" => {
            let raw = inner.unwrap_or("");
            let _ = write!(out, "<span class=\"math\">{}</span>", escape_html(raw));
        }
        // `@code` never reaches the emitter as a shortcode: the parser turns
        // it into a `Block::CodeBlock` so the body stays verbatim.
        _ => {
            let _ = write!(
                out,
                "<div class=\"shortcode-{}\">{}</div>",
                escape_attr(name),
                inner.unwrap_or("")
            );
        }
    }
}

fn collect_footnotes(doc: &Document) -> Vec<Vec<Inline>> {
    let mut out = Vec::new();
    for b in &doc.blocks {
        collect_block(b, &mut out);
    }
    out
}

fn collect_block(b: &Block, out: &mut Vec<Vec<Inline>>) {
    match b {
        Block::Heading { content, .. } | Block::Paragraph { content, .. } => {
            for n in content {
                collect_inline(n, out);
            }
        }
        Block::List { items, .. } => {
            for it in items {
                for n in &it.content {
                    collect_inline(n, out);
                }
                for c in &it.children {
                    collect_block(c, out);
                }
            }
        }
        Block::Blockquote { children, .. } | Block::BlockShortcode { children, .. } => {
            for c in children {
                collect_block(c, out);
            }
        }
        Block::Table { header, rows, .. } => {
            for cell in &header.cells {
                for n in cell {
                    collect_inline(n, out);
                }
            }
            for row in rows {
                for cell in &row.cells {
                    for n in cell {
                        collect_inline(n, out);
                    }
                }
            }
        }
        Block::DefinitionList { items, .. } => {
            for it in items {
                for n in &it.term {
                    collect_inline(n, out);
                }
                for n in &it.definition {
                    collect_inline(n, out);
                }
            }
        }
        Block::CodeBlock { .. } | Block::HorizontalRule { .. } => {}
    }
}

fn collect_inline(node: &Inline, out: &mut Vec<Vec<Inline>>) {
    match node {
        Inline::Bold { content, .. }
        | Inline::Italic { content, .. }
        | Inline::Underline { content, .. }
        | Inline::Strike { content, .. } => {
            for n in content {
                collect_inline(n, out);
            }
        }
        Inline::Shortcode { name, content, .. } => {
            if name == "footnote" {
                if let Some(c) = content {
                    out.push(c.clone());
                }
                // Don't recurse into the body: nested footnote refs inside
                // a footnote body render as plain text and are not numbered.
                return;
            }
            if let Some(c) = content {
                for n in c {
                    collect_inline(n, out);
                }
            }
        }
        _ => {}
    }
}

fn emit_footnotes_section(
    footnotes: &[Vec<Inline>],
    reg: &Registry,
    resolved_refs: &std::collections::BTreeMap<crate::span::Span, crate::ast::ResolvedRef>,
    out: &mut String,
) {
    out.push_str("<hr class=\"footnotes-sep\">\n<ol class=\"footnotes\">\n");
    for (i, body) in footnotes.iter().enumerate() {
        let n = i + 1;
        let _ = write!(out, "<li id=\"fn-{}\">", n);
        let mut ctx = Ctx {
            reg,
            counter: 0,
            in_footnote: true,
            resolved_refs,
        };
        render_inline_seq(body, &mut ctx, out);
        let _ = write!(
            out,
            " <a href=\"#fn-ref-{}\" class=\"fn-back\">\u{21A9}</a></li>\n",
            n
        );
    }
    out.push_str("</ol>\n");
}

fn expand_template(tpl: &str, args: &ShortArgs, content: &str) -> String {
    let mut out = String::new();
    let bytes = tpl.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' && bytes.get(i + 1) == Some(&b'{') {
            if let Some(rel) = tpl[i + 2..].find("}}") {
                // Literal text since the last placeholder is copied as a
                // slice, which keeps multibyte UTF-8 intact (a byte-at-a-
                // time `push(b as char)` would mangle it).
                out.push_str(&tpl[start..i]);
                let key = tpl[i + 2..i + 2 + rel].trim();
                if key == "content" {
                    out.push_str(content);
                } else if let Some(rest) = key.strip_prefix("args.") {
                    if let Some(v) = args.keyword.get(rest).and_then(|v| v.as_str()) {
                        escape_html_into(&mut out, v);
                    }
                }
                i = i + 2 + rel + 2;
                start = i;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&tpl[start..]);
    out
}

/// Escape `s` directly into `out`: unescaped stretches are copied in bulk
/// instead of char-by-char, and no intermediate String is allocated. The
/// escaped bytes are all ASCII, so scanning bytes is UTF-8 safe.
fn escape_html_into(out: &mut String, s: &str) {
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let rep = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            _ => continue,
        };
        out.push_str(&s[start..i]);
        out.push_str(rep);
        start = i + 1;
    }
    out.push_str(&s[start..]);
}

fn escape_html(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    escape_html_into(&mut o, s);
    o
}

fn escape_attr(s: &str) -> String {
    escape_html(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::parse;
    use crate::span::SourceMap;

    fn render_html(input: &str) -> String {
        let src = SourceMap::new("d.brf", input);
        let toks = lex(&src).unwrap();
        let (doc, diags) = parse(toks, &src);
        assert!(diags.is_empty(), "{:?}", diags);
        let reg = Registry::with_builtins();
        render(&doc, &reg)
    }

    #[test]
    fn html_does_not_emit_frontmatter() {
        let out = render_html("+++\ntitle = \"hi\"\n+++\n# Doc\n");
        assert!(!out.contains("+++"), "{}", out);
        assert!(!out.contains("title"), "{}", out);
        assert!(out.contains("<h1>Doc</h1>"));
    }

    fn parse_doc(s: &str) -> crate::ast::Document {
        use crate::{lexer, parser, span::SourceMap};
        let src = SourceMap::new("t.brf", s);
        let tokens = lexer::lex(&src).expect("lex");
        let (doc, _) = parser::parse(tokens, &src);
        doc
    }

    #[test]
    fn ref_lowers_to_anchor_using_resolved_refs() {
        use crate::project::ProjectIndex;
        use crate::resolve::{ResolveProject, resolve_with_project};
        use std::collections::BTreeSet;
        use std::path::PathBuf;

        let src = "See @ref[other.brf#top](the top).\n".to_string();
        let mut doc = parse_doc(&src);
        let mut idx = ProjectIndex {
            root: PathBuf::from("/tmp/p"),
            ..Default::default()
        };
        idx.anchors
            .insert("other.brf".to_string(), BTreeSet::from(["top".into()]));
        let p = ResolveProject {
            index: &idx,
            current: &PathBuf::from("here.brf"),
        };
        let reg = crate::shortcode::Registry::with_builtins();
        let _ = resolve_with_project(&mut doc, &reg, Some(&p));
        let html = render(&doc, &reg);
        assert!(
            html.contains("<a href=\"other.html#top\">the top</a>"),
            "got: {}",
            html
        );
    }

    #[test]
    fn ref_without_anchor_lowers_to_html_with_no_fragment() {
        use crate::project::ProjectIndex;
        use crate::resolve::{ResolveProject, resolve_with_project};
        use std::collections::BTreeSet;
        use std::path::PathBuf;
        let mut doc = parse_doc("See @ref[a/b.brf](title).\n");
        let mut idx = ProjectIndex::default();
        idx.anchors.insert("a/b.brf".to_string(), BTreeSet::new());
        let p = ResolveProject {
            index: &idx,
            current: &PathBuf::from("here.brf"),
        };
        let reg = crate::shortcode::Registry::with_builtins();
        let _ = resolve_with_project(&mut doc, &reg, Some(&p));
        let html = render(&doc, &reg);
        assert!(
            html.contains("<a href=\"a/b.html\">title</a>"),
            "got: {}",
            html
        );
    }

    #[test]
    fn unresolved_ref_falls_back_to_display_text() {
        // No resolve_with_project call → resolved_refs stays empty.
        let doc = parse_doc("See @ref[any.brf](fallback).\n");
        let reg = crate::shortcode::Registry::with_builtins();
        let html = render(&doc, &reg);
        assert!(html.contains("fallback"), "got: {}", html);
        assert!(
            !html.contains("<a "),
            "must not emit a broken link: {}",
            html
        );
    }

    #[test]
    fn dl_renders_as_dl_dt_dd() {
        use crate::ast::{Block, DefinitionItem, Document, Inline, ShortArgs};
        use crate::span::Span;
        let doc = Document {
            blocks: vec![Block::DefinitionList {
                args: ShortArgs::default(),
                items: vec![DefinitionItem {
                    term: vec![Inline::Text {
                        value: "Term".into(),
                        span: Span::DUMMY,
                    }],
                    definition: vec![Inline::Text {
                        value: "Definition.".into(),
                        span: Span::DUMMY,
                    }],
                    span: Span::DUMMY,
                }],
                span: Span::DUMMY,
            }],
            metadata: None,
            resolved_refs: Default::default(),
        };
        let reg = Registry::with_builtins();
        let out = render(&doc, &reg);
        assert!(out.contains("<dl>"), "got: {}", out);
        assert!(out.contains("<dt>Term</dt>"), "got: {}", out);
        assert!(out.contains("<dd>Definition.</dd>"), "got: {}", out);
        assert!(out.contains("</dl>"), "got: {}", out);
    }
}
