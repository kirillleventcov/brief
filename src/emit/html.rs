use crate::ast::{Block, Document, Inline, ListItem, Row, ShortArgs};
use crate::shortcode::{ArgValue, Registry};
use std::fmt::Write;

pub fn render(doc: &Document, reg: &Registry) -> String {
    let footnotes = collect_footnotes(doc);
    let mut ctx = Ctx {
        reg,
        counter: 0,
        in_footnote: false,
    };
    let mut out = String::new();
    for b in &doc.blocks {
        render_block(b, &mut ctx, &mut out);
    }
    if !footnotes.is_empty() {
        emit_footnotes_section(&footnotes, reg, &mut out);
    }
    out
}

struct Ctx<'a> {
    reg: &'a Registry,
    counter: u32,
    in_footnote: bool,
}

fn render_block(block: &Block, ctx: &mut Ctx, out: &mut String) {
    match block {
        Block::Heading { level, content, .. } => {
            let _ = write!(out, "<h{}>", level);
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
            let _ = writeln!(out, "<{}>", tag);
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
            out.push_str(&escape_html(body));
            out.push_str("</code></pre>\n");
        }
        Block::Table {
            args, header, rows, ..
        } => {
            render_table(args, header, rows, ctx, out);
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
    out.push_str("<li>");
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
    let aligns: Vec<&str> = if let Some(ArgValue::Array(a)) = args.keyword.get("align") {
        a.iter()
            .map(|v| match v {
                ArgValue::Ident(s) | ArgValue::Str(s) => s.as_str(),
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
        Inline::Text { value, .. } => out.push_str(&escape_html(value)),
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
            out.push_str(&escape_html(value));
            out.push_str("</code>");
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
            let _ = write!(
                out,
                "<a href=\"{}\">{}</a>",
                escape_attr(url),
                inner.unwrap_or("")
            );
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
        "code" => {
            let lang = args
                .keyword
                .get("lang")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let body = inner.unwrap_or("");
            if !lang.is_empty() {
                let _ = write!(
                    out,
                    "<pre><code class=\"language-{}\">{}</code></pre>\n",
                    escape_attr(lang),
                    escape_html(body)
                );
            } else {
                let _ = write!(out, "<pre><code>{}</code></pre>\n", escape_html(body));
            }
        }
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

fn emit_footnotes_section(footnotes: &[Vec<Inline>], reg: &Registry, out: &mut String) {
    out.push_str("<hr class=\"footnotes-sep\">\n<ol class=\"footnotes\">\n");
    for (i, body) in footnotes.iter().enumerate() {
        let n = i + 1;
        let _ = write!(out, "<li id=\"fn-{}\">", n);
        let mut ctx = Ctx {
            reg,
            counter: 0,
            in_footnote: true,
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
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' && bytes.get(i + 1) == Some(&b'{') {
            if let Some(rel) = tpl[i + 2..].find("}}") {
                let key = tpl[i + 2..i + 2 + rel].trim();
                if key == "content" {
                    out.push_str(content);
                } else if let Some(rest) = key.strip_prefix("args.") {
                    if let Some(v) = args.keyword.get(rest).and_then(|v| v.as_str()) {
                        out.push_str(&escape_html(v));
                    }
                }
                i = i + 2 + rel + 2;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn escape_html(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            _ => o.push(c),
        }
    }
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
}
