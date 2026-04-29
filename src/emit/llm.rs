use crate::ast::{Block, Document, Inline, Row, ShortArgs};
use crate::shortcode::Registry;
use std::fmt::Write;

#[derive(Clone, Debug, Default)]
pub struct Opts {
    pub strip_emphasis: bool,
    pub keep_table_rule: bool,
    pub keep_asset_urls: bool,
}

pub fn render(doc: &Document, reg: &Registry, opts: &Opts) -> String {
    let footnotes = collect_footnotes(doc);
    let mut out = String::new();
    let mut ctx = Ctx {
        reg,
        opts,
        counter: 0,
        in_footnote: false,
    };
    for b in &doc.blocks {
        render_block(b, &mut ctx, &mut out, 0);
    }
    if !footnotes.is_empty() {
        emit_footnotes_section(&footnotes, reg, opts, &mut out);
    }
    let mut collapsed = String::with_capacity(out.len());
    let mut nl_run = 0;
    for c in out.chars() {
        if c == '\n' {
            nl_run += 1;
            if nl_run <= 2 {
                collapsed.push(c);
            }
        } else {
            nl_run = 0;
            collapsed.push(c);
        }
    }
    collapsed
}

struct Ctx<'a> {
    reg: &'a Registry,
    opts: &'a Opts,
    counter: u32,
    in_footnote: bool,
}

fn render_block(b: &Block, ctx: &mut Ctx, out: &mut String, indent: usize) {
    let pad: String = std::iter::repeat(' ').take(indent).collect();
    match b {
        Block::Heading { level, content, .. } => {
            let hashes: String = std::iter::repeat('#').take(*level as usize).collect();
            let _ = write!(out, "{} ", hashes);
            render_inline_seq(content, ctx, out);
            out.push('\n');
        }
        Block::Paragraph { content, .. } => {
            if content.is_empty() {
                return;
            }
            out.push_str(&pad);
            render_inline_seq(content, ctx, out);
            out.push('\n');
        }
        Block::List { ordered, items, .. } => {
            for (i, it) in items.iter().enumerate() {
                let marker = if *ordered {
                    format!("{}.", i + 1)
                } else {
                    "-".to_string()
                };
                let _ = write!(out, "{}{} ", pad, marker);
                render_inline_seq(&it.content, ctx, out);
                out.push('\n');
                for c in &it.children {
                    render_block(c, ctx, out, indent + 1);
                }
            }
        }
        Block::Blockquote { children, .. } => {
            for c in children {
                let mut s = String::new();
                render_block(c, ctx, &mut s, 0);
                for line in s.lines() {
                    out.push_str("> ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        Block::CodeBlock { lang, body, .. } => {
            out.push_str("```");
            if let Some(l) = lang {
                out.push_str(l);
            }
            out.push('\n');
            out.push_str(body);
            out.push('\n');
            out.push_str("```\n");
        }
        Block::Table { header, rows, .. } => render_table(header, rows, ctx, out),
        Block::HorizontalRule { .. } => out.push_str("---\n"),
        Block::BlockShortcode {
            name,
            args,
            children,
            ..
        } => {
            render_block_shortcode_llm(name, args, children, ctx, out, indent);
        }
    }
}

fn render_table(header: &Row, rows: &[Row], ctx: &mut Ctx, out: &mut String) {
    let mut row_strs: Vec<Vec<String>> = Vec::new();
    let mut h: Vec<String> = Vec::new();
    for c in &header.cells {
        let mut s = String::new();
        render_inline_seq_to(c, ctx, &mut s);
        h.push(s);
    }
    row_strs.push(h);
    for r in rows {
        let mut row: Vec<String> = Vec::new();
        for c in &r.cells {
            let mut s = String::new();
            render_inline_seq_to(c, ctx, &mut s);
            row.push(s);
        }
        row_strs.push(row);
    }
    // The parser flags table-column mismatch as a diagnostic, but the
    // renderer must still be panic-free on any AST it's handed. Size column
    // widths to the widest row, not the header.
    let cols = row_strs.iter().map(|r| r.len()).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            row_strs
                .iter()
                .map(|r| r.get(c).map(|s| s.chars().count()).unwrap_or(0))
                .max()
                .unwrap_or(0)
        })
        .collect();
    for (i, row) in row_strs.iter().enumerate() {
        out.push('|');
        for (c, cell) in row.iter().enumerate() {
            let w = widths.get(c).copied().unwrap_or(0);
            let _ = write!(out, " {:width$} |", cell, width = w);
        }
        out.push('\n');
        if i == 0 && ctx.opts.keep_table_rule {
            out.push('|');
            for w in &widths {
                let dashes: String = std::iter::repeat('-').take(*w + 2).collect();
                out.push_str(&dashes);
                out.push('|');
            }
            out.push('\n');
        }
    }
}

fn render_block_shortcode_llm(
    name: &str,
    args: &ShortArgs,
    children: &[Block],
    ctx: &mut Ctx,
    out: &mut String,
    indent: usize,
) {
    if let Some(sc) = ctx.reg.get(name) {
        if let Some(t) = &sc.template_llm {
            let mut inner = String::new();
            for c in children {
                render_block(c, ctx, &mut inner, indent);
            }
            let r = expand_template_llm(t, args, &inner);
            out.push_str(&r);
            return;
        }
    }
    match name {
        "callout" => {
            let kind = args
                .keyword
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("info");
            let _ = writeln!(out, "[!{}]", kind);
            for c in children {
                render_block(c, ctx, out, indent);
            }
            let _ = writeln!(out, "[/!]");
        }
        "math" => {
            let mut s = String::new();
            for c in children {
                render_block(c, ctx, &mut s, indent);
            }
            let _ = writeln!(out, "$${}$$", s.trim());
        }
        _ => {
            let _ = writeln!(out, "@{}", name);
            for c in children {
                render_block(c, ctx, out, indent);
            }
            let _ = writeln!(out, "@end");
        }
    }
}

fn render_inline_seq(seq: &[Inline], ctx: &mut Ctx, out: &mut String) {
    for n in seq {
        render_inline(n, ctx, out);
    }
}

fn render_inline_seq_to(seq: &[Inline], ctx: &mut Ctx, out: &mut String) {
    render_inline_seq(seq, ctx, out)
}

fn render_inline(node: &Inline, ctx: &mut Ctx, out: &mut String) {
    match node {
        Inline::Text { value, .. } => out.push_str(value),
        Inline::HardBreak { .. } => out.push('\n'),
        Inline::Bold { content, .. } => emph_wrap(content, ctx, out, '*'),
        Inline::Italic { content, .. } => emph_wrap(content, ctx, out, '_'),
        Inline::Underline { content, .. } => emph_wrap(content, ctx, out, '+'),
        Inline::Strike { content, .. } => emph_wrap(content, ctx, out, '~'),
        Inline::InlineCode { value, .. } => {
            out.push('`');
            out.push_str(value);
            out.push('`');
        }
        Inline::Shortcode {
            name,
            args,
            content,
            ..
        } => {
            // Footnote refs are auto-numbered; the body is emitted in a
            // dedicated definitions section appended to the document. Inside
            // a footnote body we degrade nested footnote refs to plain
            // bracketed text so document-level numbering stays linear.
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
                let _ = write!(out, "[^{}]", ctx.counter);
                return;
            }
            render_inline_shortcode_llm(name, args, content.as_deref(), ctx, out);
        }
    }
}

fn emph_wrap(content: &[Inline], ctx: &mut Ctx, out: &mut String, m: char) {
    if ctx.opts.strip_emphasis {
        render_inline_seq(content, ctx, out);
    } else {
        out.push(m);
        render_inline_seq(content, ctx, out);
        out.push(m);
    }
}

fn render_inline_shortcode_llm(
    name: &str,
    args: &ShortArgs,
    content: Option<&[Inline]>,
    ctx: &mut Ctx,
    out: &mut String,
) {
    let inner_string = content.map(|c| {
        let mut s = String::new();
        render_inline_seq(c, ctx, &mut s);
        s
    });
    if let Some(sc) = ctx.reg.get(name) {
        if let Some(t) = &sc.template_llm {
            let r = expand_template_llm(t, args, inner_string.as_deref().unwrap_or(""));
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
                .unwrap_or("");
            let text = inner_string.as_deref().unwrap_or("");
            let _ = write!(out, "[{}]({})", text, url);
        }
        "image" => {
            let alt = args
                .keyword
                .get("alt")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if ctx.opts.keep_asset_urls {
                let src = args
                    .keyword
                    .get("src")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let _ = write!(out, "[image: {} {}]", alt, src);
            } else {
                let _ = write!(out, "[image: {}]", alt);
            }
        }
        "kbd" => {
            let _ = write!(out, "[kbd:{}]", inner_string.as_deref().unwrap_or(""));
        }
        "math" => {
            let _ = write!(out, "${}$", inner_string.as_deref().unwrap_or(""));
        }
        _ => {
            let _ = write!(out, "@{}", name);
            if let Some(s) = inner_string {
                let _ = write!(out, "[{}]", s);
            }
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
    opts: &Opts,
    out: &mut String,
) {
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    for (i, body) in footnotes.iter().enumerate() {
        let n = i + 1;
        let _ = write!(out, "[^{}]: ", n);
        let mut ctx = Ctx {
            reg,
            opts,
            counter: 0,
            in_footnote: true,
        };
        render_inline_seq(body, &mut ctx, out);
        out.push('\n');
    }
}

fn expand_template_llm(tpl: &str, args: &ShortArgs, content: &str) -> String {
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
                        out.push_str(v);
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
