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
    let mut out = String::new();
    for b in &doc.blocks {
        render_block(b, reg, opts, &mut out, 0);
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

fn render_block(b: &Block, reg: &Registry, opts: &Opts, out: &mut String, indent: usize) {
    let pad: String = std::iter::repeat(' ').take(indent).collect();
    match b {
        Block::Heading { level, content, .. } => {
            let hashes: String = std::iter::repeat('#').take(*level as usize).collect();
            let _ = write!(out, "{} ", hashes);
            render_inline_seq(content, reg, opts, out);
            out.push('\n');
        }
        Block::Paragraph { content, .. } => {
            if content.is_empty() {
                return;
            }
            out.push_str(&pad);
            render_inline_seq(content, reg, opts, out);
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
                render_inline_seq(&it.content, reg, opts, out);
                out.push('\n');
                for c in &it.children {
                    render_block(c, reg, opts, out, indent + 1);
                }
            }
        }
        Block::Blockquote { children, .. } => {
            for c in children {
                let mut s = String::new();
                render_block(c, reg, opts, &mut s, 0);
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
        Block::Table { header, rows, .. } => render_table(header, rows, reg, opts, out),
        Block::HorizontalRule { .. } => out.push_str("---\n"),
        Block::BlockShortcode {
            name,
            args,
            children,
            ..
        } => {
            render_block_shortcode_llm(name, args, children, reg, opts, out, indent);
        }
    }
}

fn render_table(header: &Row, rows: &[Row], reg: &Registry, opts: &Opts, out: &mut String) {
    let mut row_strs: Vec<Vec<String>> = Vec::new();
    let mut h: Vec<String> = Vec::new();
    for c in &header.cells {
        let mut s = String::new();
        render_inline_seq(c, reg, opts, &mut s);
        h.push(s);
    }
    row_strs.push(h);
    for r in rows {
        let mut row: Vec<String> = Vec::new();
        for c in &r.cells {
            let mut s = String::new();
            render_inline_seq(c, reg, opts, &mut s);
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
        if i == 0 && opts.keep_table_rule {
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
    reg: &Registry,
    opts: &Opts,
    out: &mut String,
    indent: usize,
) {
    if let Some(sc) = reg.get(name) {
        if let Some(t) = &sc.template_llm {
            let mut inner = String::new();
            for c in children {
                render_block(c, reg, opts, &mut inner, indent);
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
                render_block(c, reg, opts, out, indent);
            }
            let _ = writeln!(out, "[/!]");
        }
        "math" => {
            let mut s = String::new();
            for c in children {
                render_block(c, reg, opts, &mut s, indent);
            }
            let _ = writeln!(out, "$${}$$", s.trim());
        }
        _ => {
            let _ = writeln!(out, "@{}", name);
            for c in children {
                render_block(c, reg, opts, out, indent);
            }
            let _ = writeln!(out, "@end");
        }
    }
}

fn render_inline_seq(seq: &[Inline], reg: &Registry, opts: &Opts, out: &mut String) {
    for n in seq {
        render_inline(n, reg, opts, out);
    }
}

fn render_inline(node: &Inline, reg: &Registry, opts: &Opts, out: &mut String) {
    match node {
        Inline::Text { value, .. } => out.push_str(value),
        Inline::HardBreak { .. } => out.push('\n'),
        Inline::Bold { content, .. } => emph_wrap(content, reg, opts, out, '*'),
        Inline::Italic { content, .. } => emph_wrap(content, reg, opts, out, '_'),
        Inline::Underline { content, .. } => emph_wrap(content, reg, opts, out, '+'),
        Inline::Strike { content, .. } => emph_wrap(content, reg, opts, out, '~'),
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
            render_inline_shortcode_llm(name, args, content.as_deref(), reg, opts, out);
        }
    }
}

fn emph_wrap(content: &[Inline], reg: &Registry, opts: &Opts, out: &mut String, m: char) {
    if opts.strip_emphasis {
        render_inline_seq(content, reg, opts, out);
    } else {
        out.push(m);
        render_inline_seq(content, reg, opts, out);
        out.push(m);
    }
}

fn render_inline_shortcode_llm(
    name: &str,
    args: &ShortArgs,
    content: Option<&[Inline]>,
    reg: &Registry,
    opts: &Opts,
    out: &mut String,
) {
    let inner_string = content.map(|c| {
        let mut s = String::new();
        render_inline_seq(c, reg, opts, &mut s);
        s
    });
    if let Some(sc) = reg.get(name) {
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
            if opts.keep_asset_urls {
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
        "footnote" => {
            let _ = write!(out, "(fn: {})", inner_string.as_deref().unwrap_or(""));
        }
        _ => {
            let _ = write!(out, "@{}", name);
            if let Some(s) = inner_string {
                let _ = write!(out, "[{}]", s);
            }
        }
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
