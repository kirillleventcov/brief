use crate::ast::{Block, CodeAttrs, Document, Inline, Row, ShortArgs};
use crate::minify::{self, MinifyOptions, MinifyWarning};
use crate::shortcode::Registry;
use std::fmt::Write;

/// A minified code block longer than this many source lines triggers a B0702
/// warning that the LLM consumer cannot reference original line numbers.
const LONG_BLOCK_LINE_THRESHOLD: usize = 50;

#[derive(Clone, Debug)]
pub struct Opts {
    pub strip_emphasis: bool,
    pub keep_table_rule: bool,
    pub keep_asset_urls: bool,
    pub keep_metadata: bool,
    /// Master toggle for code-block minification. When false, every code
    /// block is emitted verbatim regardless of tag or `@minify`.
    pub minify_code_blocks: bool,
    /// Lowercased language tags eligible for the configured minifiers.
    pub minify_languages: Vec<String>,
    /// Keep the surrounding ```lang fence around minified output. When
    /// false, only the minified body is emitted (no fence).
    pub preserve_code_fences: bool,
}

impl Default for Opts {
    fn default() -> Self {
        // The Opts default is used in unit tests and in fall-back code paths
        // when no `brief.toml` is on disk. Keeping it in sync with the
        // canonical default in `config::default_minify_languages` matters
        // because the LLM emit pass is the one that actually consults this
        // list to decide whether to attempt minification on a tag the
        // dispatcher knows about.
        Opts {
            strip_emphasis: false,
            keep_table_rule: false,
            keep_asset_urls: false,
            keep_metadata: false,
            minify_code_blocks: true,
            minify_languages: vec![
                "json".into(),
                "jsonl".into(),
                "rust".into(),
                "rs".into(),
                "c".into(),
                "h".into(),
                "cpp".into(),
                "c++".into(),
                "cc".into(),
                "cxx".into(),
                "hpp".into(),
                "hxx".into(),
                "java".into(),
                "go".into(),
                "javascript".into(),
                "js".into(),
                "typescript".into(),
                "ts".into(),
                "sql".into(),
            ],
            preserve_code_fences: true,
        }
    }
}

pub fn render(doc: &Document, reg: &Registry, opts: &Opts) -> (String, Vec<String>) {
    let footnotes = collect_footnotes(doc);
    let mut out = String::new();
    let frontmatter_minify_code = doc
        .metadata
        .as_ref()
        .and_then(|m| m.get("minify_code"))
        .and_then(|v| v.as_bool());
    if opts.keep_metadata {
        if let Some(meta) = &doc.metadata {
            // Re-serialize the metadata table to preserve it as Brief
            // frontmatter at the top of the LLM output.
            let body = toml::to_string(meta).unwrap_or_default();
            out.push_str("+++\n");
            out.push_str(&body);
            if !body.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("+++\n\n");
        }
    }
    let mut ctx = Ctx {
        reg,
        opts,
        counter: 0,
        in_footnote: false,
        warnings: Vec::new(),
        frontmatter_minify_code,
        resolved_refs: &doc.resolved_refs,
    };
    for b in &doc.blocks {
        render_block(b, &mut ctx, &mut out, 0);
    }
    if !footnotes.is_empty() {
        emit_footnotes_section(&footnotes, reg, opts, &doc.resolved_refs, &mut out);
    }
    let warnings = ctx.warnings;
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
    (collapsed, warnings)
}

struct Ctx<'a> {
    reg: &'a Registry,
    opts: &'a Opts,
    counter: u32,
    in_footnote: bool,
    warnings: Vec<String>,
    frontmatter_minify_code: Option<bool>,
    resolved_refs: &'a std::collections::BTreeMap<crate::span::Span, crate::ast::ResolvedRef>,
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
            use crate::ast::TaskState;
            for (i, it) in items.iter().enumerate() {
                let marker = if *ordered {
                    format!("{}.", i + 1)
                } else {
                    "-".to_string()
                };
                let _ = write!(out, "{}{} ", pad, marker);
                if let Some(state) = it.task {
                    out.push_str(match state {
                        TaskState::Done => "[x] ",
                        TaskState::Todo => "[ ] ",
                    });
                }
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
        Block::CodeBlock {
            lang, body, attrs, ..
        } => {
            emit_code_block(lang.as_deref(), body, attrs, ctx, out);
        }
        Block::Table { header, rows, .. } => render_table(header, rows, ctx, out),
        Block::DefinitionList { items, .. } => {
            out.push_str("@dl\n");
            for it in items {
                render_inline_seq(&it.term, ctx, out);
                out.push('\n');
                out.push_str(": ");
                render_inline_seq(&it.definition, ctx, out);
                out.push('\n');
            }
            out.push_str("@end\n");
        }
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

fn emit_code_block(
    lang: Option<&str>,
    body: &str,
    attrs: &CodeAttrs,
    ctx: &mut Ctx,
    out: &mut String,
) {
    let minified = try_minify(lang, body, attrs, ctx);
    let body_to_emit = minified.as_deref().unwrap_or(body);
    if ctx.opts.preserve_code_fences {
        out.push_str("```");
        if let Some(l) = lang {
            out.push_str(l);
        }
        out.push('\n');
        out.push_str(body_to_emit);
        out.push('\n');
        out.push_str("```\n");
    } else {
        out.push_str(body_to_emit);
        out.push('\n');
    }
}

fn try_minify(lang: Option<&str>, body: &str, attrs: &CodeAttrs, ctx: &mut Ctx) -> Option<String> {
    if attrs.nominify {
        return None;
    }
    let lang = lang?;
    let lang_lc = lang.to_ascii_lowercase();

    let force_minify = attrs.minify || attrs.keep_comments;

    // Refusal gate (B0704). Only emitted when the user is trying to
    // minify the block — either via an explicit attribute, or because the
    // language is on the allowlist (i.e., it was deliberately enabled in
    // brief.toml). We never error on a bare ```python``` block that was
    // never opted in.
    if let Some(reason) = minify::refusal_reason(&lang_lc) {
        let in_allowlist = ctx
            .opts
            .minify_languages
            .iter()
            .any(|x| x.eq_ignore_ascii_case(&lang_lc));
        if force_minify || in_allowlist {
            ctx.warnings.push(format!(
                "error[B0704]: language `{}` cannot be minified — {}",
                lang, reason
            ));
        }
        return None;
    }

    // Three layers of opt-out, in priority order: per-block @minify (or
    // @minify-keep-comments) forces minification, overriding all opt-outs
    // except @nominify; then frontmatter `minify_code = false`; then
    // config `minify_code_blocks`.
    if !force_minify {
        if let Some(false) = ctx.frontmatter_minify_code {
            return None;
        }
        if !ctx.opts.minify_code_blocks {
            return None;
        }
    }

    // Allowlist gate. Even with @minify, only languages with registered
    // minifiers are processed; everything else falls back to verbatim.
    let in_allowlist = ctx
        .opts
        .minify_languages
        .iter()
        .any(|x| x.eq_ignore_ascii_case(&lang_lc));
    if !in_allowlist && !force_minify {
        return None;
    }
    if !minify::is_supported(&lang_lc) {
        return None;
    }

    let mopts = MinifyOptions {
        keep_comments: attrs.keep_comments,
    };
    match minify::minify(&lang_lc, body, &mopts) {
        Ok(out) => {
            for w in &out.warnings {
                match w {
                    MinifyWarning::LineCommentConverted => {
                        ctx.warnings.push(format!(
                            "warning[B0703]: line comment converted to block form for minification in `{}` block; verify no `*/` content",
                            lang
                        ));
                    }
                }
            }
            let n_lines = body.lines().count();
            if n_lines > LONG_BLOCK_LINE_THRESHOLD {
                ctx.warnings.push(format!(
                    "warning[B0702]: minified code block was originally {} lines. LLM consumers cannot reference specific lines after minification. Consider @nominify if line references matter.",
                    n_lines
                ));
            }
            Some(out.body)
        }
        Err(e) => {
            ctx.warnings.push(format!(
                "warning[B0701]: code block tagged `{}` did not parse; emitted verbatim ({})",
                lang, e.message
            ));
            None
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
        "details" => {
            let summary = args
                .keyword
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let _ = writeln!(out, "[details: \"{}\"]", summary);
            for c in children {
                render_block(c, ctx, out, indent);
            }
            let _ = writeln!(out, "[/details]");
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
            span,
        } if name == "ref" => {
            let resolved = ctx.resolved_refs.get(span);
            let display = if let Some(r) = resolved {
                r.display.clone()
            } else if let Some(s) = args.keyword.get("title").and_then(|v| v.as_str()) {
                s.to_string()
            } else if let Some(s) = args.positional.first().and_then(|v| v.as_str()) {
                s.to_string()
            } else if let Some([Inline::Text { value, .. }]) = content.as_deref() {
                value.clone()
            } else {
                String::new()
            };
            out.push_str(&display);
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
            let title = args.keyword.get("title").and_then(|v| v.as_str());
            if let Some(t) = title {
                let _ = write!(out, "[{}]({} \"{}\")", text, url, t);
            } else {
                let _ = write!(out, "[{}]({})", text, url);
            }
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
        "sub" => {
            let _ = write!(out, "[sub:{}]", inner_string.as_deref().unwrap_or(""));
        }
        "sup" => {
            let _ = write!(out, "[sup:{}]", inner_string.as_deref().unwrap_or(""));
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
    resolved_refs: &std::collections::BTreeMap<crate::span::Span, crate::ast::ResolvedRef>,
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
            warnings: Vec::new(),
            frontmatter_minify_code: None,
            resolved_refs,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::parse;
    use crate::span::SourceMap;

    fn render_with(input: &str, opts: Opts) -> (String, Vec<String>) {
        let src = SourceMap::new("d.brf", input);
        let toks = lex(&src).unwrap();
        let (doc, diags) = parse(toks, &src);
        assert!(diags.is_empty(), "{:?}", diags);
        let reg = Registry::with_builtins();
        render(&doc, &reg, &opts)
    }

    fn render_default(input: &str) -> String {
        render_with(input, Opts::default()).0
    }

    fn opts_with_keep_metadata() -> Opts {
        Opts {
            keep_metadata: true,
            ..Opts::default()
        }
    }

    #[test]
    fn llm_strips_frontmatter_by_default() {
        let out = render_default("+++\ntitle = \"hi\"\n+++\n# Doc\n");
        assert!(!out.contains("+++"), "{}", out);
        assert!(!out.contains("title"), "{}", out);
        assert!(out.contains("# Doc"));
    }

    #[test]
    fn llm_keeps_frontmatter_with_flag() {
        let (out, _) = render_with(
            "+++\ntitle = \"hi\"\n+++\n# Doc\n",
            opts_with_keep_metadata(),
        );
        assert!(
            out.starts_with("+++\n"),
            "starts with: {:?}",
            &out[..20.min(out.len())]
        );
        assert!(out.contains("title"));
        assert!(out.contains("# Doc"));
        let close_pos = out.find("\n+++\n").expect("closing +++ missing");
        let doc_pos = out.find("# Doc").expect("body missing");
        assert!(close_pos < doc_pos, "closing +++ must precede body");
        assert!(
            out.contains("+++\n\n"),
            "blank line after closing +++ missing"
        );
    }

    #[test]
    fn llm_keep_metadata_no_op_when_no_metadata() {
        let (out, _) = render_with("# Doc\n", opts_with_keep_metadata());
        assert!(!out.contains("+++"), "{}", out);
        assert!(out.contains("# Doc"));
    }

    #[test]
    fn json_block_minified_by_default() {
        let (out, w) = render_with(
            "```json\n{\n  \"a\": 1,\n  \"b\": [1, 2, 3]\n}\n```\n",
            Opts::default(),
        );
        assert!(w.is_empty(), "unexpected warnings: {:?}", w);
        assert!(out.contains("{\"a\":1,\"b\":[1,2,3]}"), "{}", out);
        assert!(
            out.contains("```json"),
            "fence preserved by default: {}",
            out
        );
    }

    #[test]
    fn json_block_with_nominify_kept_verbatim() {
        let src = "```json @nominify\n{\n  \"a\": 1\n}\n```\n";
        let (out, w) = render_with(src, Opts::default());
        assert!(w.is_empty());
        assert!(
            out.contains("\"a\": 1"),
            "must preserve whitespace: {}",
            out
        );
    }

    #[test]
    fn invalid_json_falls_back_with_warning() {
        let src = "```json\n{ not valid }\n```\n";
        let (out, w) = render_with(src, Opts::default());
        assert!(out.contains("{ not valid }"), "verbatim body: {}", out);
        assert_eq!(w.len(), 1, "expected one B0701 warning");
        assert!(w[0].contains("B0701"));
    }

    #[test]
    fn jsonl_block_minified() {
        let src = "```jsonl\n{\"a\": 1}\n{\"b\": 2}\n```\n";
        let (out, w) = render_with(src, Opts::default());
        assert!(w.is_empty(), "{:?}", w);
        assert!(out.contains("{\"a\":1}\n{\"b\":2}"), "{}", out);
    }

    #[test]
    fn rust_block_minified_in_v0_3() {
        let src = "```rust\nfn x() {\n    // hi\n    1\n}\n```\n";
        let (out, w) = render_with(src, Opts::default());
        assert!(w.is_empty(), "{:?}", w);
        assert!(out.contains("fn x(){1}"), "minified rust: {}", out);
        assert!(!out.contains("// hi"), "comment dropped: {}", out);
    }

    #[test]
    fn js_block_preserves_newlines() {
        let src = "```javascript\nfunction add(a, b) {\n    return a + b;\n}\n```\n";
        let (out, w) = render_with(src, Opts::default());
        assert!(w.is_empty(), "{:?}", w);
        assert!(
            out.contains("function add(a,b){\nreturn a+b;\n}"),
            "got: {}",
            out
        );
    }

    #[test]
    fn ts_alias_minifies() {
        let src = "```ts\nfunction f(x: number): string { return String(x); }\n```\n";
        let (out, w) = render_with(src, Opts::default());
        assert!(w.is_empty(), "{:?}", w);
        assert!(
            out.contains("function f(x:number):string{return String(x);}"),
            "got: {}",
            out
        );
    }

    #[test]
    fn sql_block_minified() {
        let src = "```sql\nSELECT *\nFROM users\nWHERE id = 1;\n```\n";
        let (out, w) = render_with(src, Opts::default());
        assert!(w.is_empty(), "{:?}", w);
        assert!(
            out.contains("SELECT*FROM users WHERE id=1;"),
            "got: {}",
            out
        );
    }

    #[test]
    fn c_preprocessor_kept_on_own_line() {
        let src = "```c\n#include <stdio.h>\nint main() { return 0; }\n```\n";
        let (out, w) = render_with(src, Opts::default());
        assert!(w.is_empty(), "{:?}", w);
        assert!(
            out.contains("#include <stdio.h>\nint main(){return 0;}"),
            "got: {}",
            out
        );
    }

    #[test]
    fn cpp_alias_minifies_with_raw_string() {
        let src = "```cpp\nconst char* s = R\"x(hi)x\";\n```\n";
        let (out, w) = render_with(src, Opts::default());
        assert!(w.is_empty(), "{:?}", w);
        assert!(out.contains("R\"x(hi)x\""), "got: {}", out);
    }

    #[test]
    fn java_block_minified() {
        let src = "```java\npublic class Foo {\n    @Override public void f() {}\n}\n```\n";
        let (out, w) = render_with(src, Opts::default());
        assert!(w.is_empty(), "{:?}", w);
        assert!(
            out.contains("public class Foo{@Override public void f(){}}"),
            "got: {}",
            out
        );
    }

    #[test]
    fn go_block_preserves_newlines() {
        let src = "```go\nfunc add(a, b int) int {\n    return a + b\n}\n```\n";
        let (out, w) = render_with(src, Opts::default());
        assert!(w.is_empty(), "{:?}", w);
        assert!(
            out.contains("func add(a,b int)int{\nreturn a+b\n}"),
            "got: {}",
            out
        );
    }

    #[test]
    fn keep_comments_emits_b0703() {
        let src = "```rust @minify-keep-comments\nfn x() {\n    // hi\n    1\n}\n```\n";
        let (out, w) = render_with(src, Opts::default());
        assert!(out.contains("/* hi*/"), "comment preserved: {}", out);
        assert!(
            w.iter().any(|s| s.contains("B0703")),
            "B0703 warning emitted: {:?}",
            w
        );
    }

    #[test]
    fn refused_language_emits_b0704() {
        // `python` is permanently refused (significant whitespace). With
        // `@minify` forcing the attempt, the LLM emit pass surfaces a
        // B0704 error and falls back to verbatim output.
        let src = "```python @minify\ndef f(x):\n    return x\n```\n";
        let (out, w) = render_with(src, Opts::default());
        assert!(out.contains("def f(x):\n    return x"), "verbatim: {}", out);
        assert!(
            w.iter().any(|s| s.contains("B0704")),
            "B0704 emitted: {:?}",
            w
        );
    }

    #[test]
    fn refused_language_silent_without_optin() {
        // No `@minify`, default allowlist excludes python — no warning.
        let opts = Opts {
            // Make sure python is NOT in the allowlist (default already
            // excludes it but be explicit for the test's intent).
            minify_languages: vec!["json".into()],
            ..Opts::default()
        };
        let src = "```python\nx = 1\n```\n";
        let (_out, w) = render_with(src, opts);
        assert!(w.is_empty(), "no warning expected: {:?}", w);
    }

    #[test]
    fn long_block_emits_b0702() {
        // Build a 60-line JSON block (each line is its own array element).
        let mut body = String::from("[\n");
        for i in 0..60 {
            body.push_str(&format!("  {}", i));
            if i < 59 {
                body.push(',');
            }
            body.push('\n');
        }
        body.push(']');
        let src = format!("```json\n{}\n```\n", body);
        let (_out, w) = render_with(&src, Opts::default());
        assert!(
            w.iter().any(|s| s.contains("B0702")),
            "B0702 emitted for long block: {:?}",
            w
        );
    }

    #[test]
    fn frontmatter_minify_code_false_disables() {
        let src = "+++\nminify_code = false\n+++\n```json\n{\"a\": 1}\n```\n";
        let (out, _) = render_with(src, Opts::default());
        // Whitespace between key and value is preserved; the minifier did
        // not run.
        assert!(out.contains("\"a\": 1"), "verbatim under override: {}", out);
    }

    #[test]
    fn frontmatter_override_can_be_force_minified() {
        // `@minify` on a block overrides the document-level disable.
        let src = "+++\nminify_code = false\n+++\n```json @minify\n{\"a\": 1}\n```\n";
        let (out, _) = render_with(src, Opts::default());
        assert!(out.contains("{\"a\":1}"), "minified anyway: {}", out);
    }

    #[test]
    fn config_disable_minification_globally() {
        let src = "```json\n{\"a\": 1}\n```\n";
        let opts = Opts {
            minify_code_blocks: false,
            ..Opts::default()
        };
        let (out, _) = render_with(src, opts);
        assert!(out.contains("\"a\": 1"), "disabled globally: {}", out);
    }

    #[test]
    fn drop_fence_with_preserve_false() {
        let src = "```json\n{\"a\": 1}\n```\n";
        let opts = Opts {
            preserve_code_fences: false,
            ..Opts::default()
        };
        let (out, _) = render_with(src, opts);
        assert!(!out.contains("```"), "fence dropped: {}", out);
        assert!(out.contains("{\"a\":1}"));
    }

    #[test]
    fn ref_renders_display_text_only_in_llm_mode() {
        use crate::project::ProjectIndex;
        use crate::resolve::{ResolveProject, resolve_with_project};
        use std::collections::BTreeSet;
        use std::path::PathBuf;

        // Parse + resolve with project so resolved_refs is populated.
        let src = "See @ref[other.brf#x](the spec).\n";
        let mut doc = {
            use crate::{lexer, parser, span::SourceMap};
            let s = SourceMap::new("t.brf", src);
            let tokens = lexer::lex(&s).expect("lex");
            let (d, _) = parser::parse(tokens, &s);
            d
        };
        let mut idx = ProjectIndex::default();
        idx.anchors
            .insert("other.brf".to_string(), BTreeSet::from(["x".into()]));
        let p = ResolveProject {
            index: &idx,
            current: &PathBuf::from("here.brf"),
        };
        let reg = crate::shortcode::Registry::with_builtins();
        let _ = resolve_with_project(&mut doc, &reg, Some(&p));

        let opts = Opts::default();
        let (out, _warnings) = render(&doc, &reg, &opts);
        assert!(out.contains("the spec"), "got: {}", out);
        assert!(
            !out.contains("other.brf"),
            "url must be dropped in LLM mode: {}",
            out
        );
        assert!(
            !out.contains("#x"),
            "anchor must be dropped in LLM mode: {}",
            out
        );
    }

    #[test]
    fn unresolved_ref_in_llm_falls_back_to_display_text() {
        let doc = {
            use crate::{lexer, parser, span::SourceMap};
            let s = SourceMap::new("t.brf", "See @ref[a.brf](just text).\n");
            let tokens = lexer::lex(&s).expect("lex");
            let (d, _) = parser::parse(tokens, &s);
            d
        };
        let reg = crate::shortcode::Registry::with_builtins();
        let opts = Opts::default();
        let (out, _) = render(&doc, &reg, &opts);
        assert!(out.contains("just text"), "got: {}", out);
        assert!(!out.contains("a.brf"));
    }

    #[test]
    fn dl_renders_verbatim_brief_form() {
        use crate::ast::{Block, DefinitionItem, Document, Inline, ShortArgs};
        use crate::span::Span;
        let doc = Document {
            blocks: vec![Block::DefinitionList {
                args: ShortArgs::default(),
                items: vec![
                    DefinitionItem {
                        term: vec![Inline::Text {
                            value: "Term1".into(),
                            span: Span::DUMMY,
                        }],
                        definition: vec![Inline::Text {
                            value: "Def1.".into(),
                            span: Span::DUMMY,
                        }],
                        span: Span::DUMMY,
                    },
                    DefinitionItem {
                        term: vec![Inline::Text {
                            value: "Term2".into(),
                            span: Span::DUMMY,
                        }],
                        definition: vec![Inline::Text {
                            value: "Def2.".into(),
                            span: Span::DUMMY,
                        }],
                        span: Span::DUMMY,
                    },
                ],
                span: Span::DUMMY,
            }],
            metadata: None,
            resolved_refs: Default::default(),
        };
        let reg = Registry::with_builtins();
        let opts = Opts::default();
        let (out, _w) = render(&doc, &reg, &opts);
        assert!(out.contains("@dl\n"), "got: {}", out);
        assert!(out.contains("Term1\n: Def1.\n"), "got: {}", out);
        assert!(out.contains("Term2\n: Def2.\n"), "got: {}", out);
        assert!(out.contains("@end\n"), "got: {}", out);
    }
}
