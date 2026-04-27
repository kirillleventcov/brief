use crate::ast::{Block, Document, Inline, ListItem, Row, ShortArgs};
use crate::shortcode::{ArgValue, Registry};
use std::fmt::Write;

pub fn render(doc: &Document, reg: &Registry) -> String {
    let mut out = String::new();
    for b in &doc.blocks {
        render_block(b, reg, &mut out);
    }
    out
}

fn render_block(block: &Block, reg: &Registry, out: &mut String) {
    match block {
        Block::Heading { level, content, .. } => {
            let _ = write!(out, "<h{}>", level);
            render_inline_seq(content, reg, out);
            let _ = writeln!(out, "</h{}>", level);
        }
        Block::Paragraph { content, .. } => {
            if content.is_empty() {
                return;
            }
            out.push_str("<p>");
            render_inline_seq(content, reg, out);
            out.push_str("</p>\n");
        }
        Block::List { ordered, items, .. } => {
            let tag = if *ordered { "ol" } else { "ul" };
            let _ = writeln!(out, "<{}>", tag);
            for it in items {
                render_item(it, reg, out);
            }
            let _ = writeln!(out, "</{}>", tag);
        }
        Block::Blockquote { children, .. } => {
            out.push_str("<blockquote>\n");
            for c in children {
                render_block(c, reg, out);
            }
            out.push_str("</blockquote>\n");
        }
        Block::CodeBlock { lang, body, .. } => {
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
            render_table(args, header, rows, reg, out);
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
                    render_block(c, reg, &mut s);
                }
                s
            };
            render_shortcode_html(name, args, Some(&inner), reg, out);
        }
    }
}

fn render_item(it: &ListItem, reg: &Registry, out: &mut String) {
    out.push_str("<li>");
    render_inline_seq(&it.content, reg, out);
    if !it.children.is_empty() {
        out.push('\n');
        for c in &it.children {
            render_block(c, reg, out);
        }
    }
    out.push_str("</li>\n");
}

fn render_table(args: &ShortArgs, header: &Row, rows: &[Row], reg: &Registry, out: &mut String) {
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
        render_inline_seq(c, reg, out);
        out.push_str("</th>");
    }
    out.push_str("</tr></thead>\n<tbody>\n");
    for r in rows {
        out.push_str("<tr>");
        for (i, c) in r.cells.iter().enumerate() {
            let a = aligns.get(i).copied().unwrap_or("left");
            let _ = write!(out, "<td style=\"text-align:{}\">", a);
            render_inline_seq(c, reg, out);
            out.push_str("</td>");
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</tbody>\n</table>\n");
}

fn render_inline_seq(seq: &[Inline], reg: &Registry, out: &mut String) {
    for n in seq {
        render_inline(n, reg, out);
    }
}

fn render_inline(node: &Inline, reg: &Registry, out: &mut String) {
    match node {
        Inline::Text { value, .. } => out.push_str(&escape_html(value)),
        Inline::HardBreak { .. } => out.push_str("<br>"),
        Inline::Bold { content, .. } => {
            out.push_str("<strong>");
            render_inline_seq(content, reg, out);
            out.push_str("</strong>");
        }
        Inline::Italic { content, .. } => {
            out.push_str("<em>");
            render_inline_seq(content, reg, out);
            out.push_str("</em>");
        }
        Inline::Underline { content, .. } => {
            out.push_str("<u>");
            render_inline_seq(content, reg, out);
            out.push_str("</u>");
        }
        Inline::Strike { content, .. } => {
            out.push_str("<s>");
            render_inline_seq(content, reg, out);
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
            let inner = content.as_ref().map(|c| {
                let mut s = String::new();
                render_inline_seq(c, reg, &mut s);
                s
            });
            render_shortcode_html(name, args, inner.as_deref(), reg, out);
        }
    }
}

fn render_shortcode_html(
    name: &str,
    args: &ShortArgs,
    inner: Option<&str>,
    reg: &Registry,
    out: &mut String,
) {
    if let Some(sc) = reg.get(name) {
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
        "footnote" => {
            let _ = write!(out, "<sup class=\"fn\">{}</sup>", inner.unwrap_or(""));
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
