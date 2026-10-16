#![no_main]

use brief::ast::{Block, Inline};
use brief::emit::{html, llm};
use brief::lexer;
use brief::parser;
use brief::resolve;
use brief::shortcode::Registry;
use brief::span::{SourceMap, Span};
use brief::validate::{self, ValidateOpts};
use libfuzzer_sys::fuzz_target;

/// Every span must slice the source cleanly: in bounds, both ends on char
/// boundaries. Editors and the LSP slice by these spans.
fn check(src: &str, span: Span) {
    let (start, end) = (span.start as usize, span.end());
    assert!(
        end <= src.len() && src.is_char_boundary(start) && src.is_char_boundary(end),
        "bad span {}..{} in {:?}",
        start,
        end,
        src
    );
}

fn check_inlines(src: &str, seq: &[Inline]) {
    for n in seq {
        match n {
            Inline::Text { span, .. }
            | Inline::InlineCode { span, .. }
            | Inline::HardBreak { span } => check(src, *span),
            Inline::Bold { content, span }
            | Inline::Italic { content, span }
            | Inline::Underline { content, span }
            | Inline::Strike { content, span } => {
                check(src, *span);
                check_inlines(src, content);
            }
            Inline::Shortcode { content, span, .. } => {
                check(src, *span);
                if let Some(c) = content {
                    check_inlines(src, c);
                }
            }
        }
    }
}

fn check_blocks(src: &str, blocks: &[Block]) {
    for b in blocks {
        match b {
            Block::Heading { content, span, .. } | Block::Paragraph { content, span } => {
                check(src, *span);
                check_inlines(src, content);
            }
            Block::List { items, span, .. } => {
                check(src, *span);
                for it in items {
                    check(src, it.span);
                    check_inlines(src, &it.content);
                    check_blocks(src, &it.children);
                }
            }
            Block::Blockquote { children, span } | Block::BlockShortcode { children, span, .. } => {
                check(src, *span);
                check_blocks(src, children);
            }
            Block::CodeBlock { span, .. } | Block::HorizontalRule { span } => check(src, *span),
            Block::Table {
                header, rows, span, ..
            } => {
                check(src, *span);
                for r in std::iter::once(header).chain(rows) {
                    check(src, r.span);
                    for c in &r.cells {
                        check_inlines(src, c);
                    }
                }
            }
            Block::DefinitionList { items, span, .. } => {
                check(src, *span);
                for it in items {
                    check(src, it.span);
                    check_inlines(src, &it.term);
                    check_inlines(src, &it.definition);
                }
            }
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    // Bound input so the fuzzer doesn't waste time on large mutations that
    // only stress allocator throughput. Real Brief files are well under this.
    if input.len() > 64 * 1024 {
        return;
    }

    let src = SourceMap::new("fuzz.brf", input);
    let tokens = match lexer::lex(&src) {
        Ok(t) => t,
        Err(d) => {
            for x in &d {
                check(input, x.span);
            }
            return;
        }
    };
    let (mut doc, mut diags) = parser::parse(tokens, &src);
    let registry = Registry::with_builtins();
    diags.extend(resolve::resolve(&mut doc, &registry));
    diags.extend(validate::validate(&doc, &ValidateOpts::default(), &src));
    check_blocks(input, &doc.blocks);
    for d in &diags {
        check(input, d.span);
    }
    let _ = brief::diag::render_all(&diags, &src);

    // Emitters must not panic on any well-typed AST the parser produces, even
    // for inputs that produced diagnostics — diagnostics are non-fatal.
    let _ = html::render(&doc, &registry);
    let _ = llm::render(&doc, &registry, &llm::Opts::default());
});
