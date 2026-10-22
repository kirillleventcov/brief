//! Span invariant oracle. Every span the pipeline produces (diagnostics and
//! AST nodes) must lie inside the source and start and end on char
//! boundaries: editors and the LSP slice the source by these spans, and an
//! off-boundary span is a panic there. Inputs are generated from
//! Brief-shaped fragments with a fixed-seed xorshift so failures replay.

use brief::ast::{Block, Inline};
use brief::diag::{self, Diagnostic};
use brief::emit::{html, llm};
use brief::lexer;
use brief::parser;
use brief::resolve;
use brief::shortcode::Registry;
use brief::span::{SourceMap, Span};
use brief::validate::{self, ValidateOptions};

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
}

/// Text that can appear anywhere on a line.
const INLINE: &[&str] = &[
    "word",
    "é",
    "日本",
    "🦀",
    " ",
    "  ",
    "\t",
    "*",
    "_",
    "+",
    "~",
    "*bold*",
    "_it é_",
    "+u+",
    "~s~",
    "*a _b *c* d_ e*",
    "`code`",
    "``a ` b``",
    "`",
    "\\",
    "\\*",
    "\\é",
    "@",
    "@kbd[Ctrl]",
    "@kbd(1 2)[x]",
    "@callout(kind: é)",
    "@link[t](https://e.com/a_(b))",
    "@link[t](https://e.com/a(b",
    "@x(\"日本",
    "@x(a: 1, a: 2)",
    "@x(-)",
    "@x([1, é])",
    "@math[\\frac{a}{b} < c]",
    "@math[\\sqrt[3]{x}]",
    "@math[x \\in [0, 1)",
    "@sup[@sub[*x*]]",
    "@footnote[note é]",
    "]",
    "[",
    "(",
    ")",
    "|",
    "/*",
    "*/",
    " /* c */ ",
    "{#anchor}",
    " {#Bad}",
    ":",
    "#",
];

/// Things that mean something at the start of a line.
const LINE_START: &[&str] = &[
    "",
    "",
    "",
    "# ",
    "## ",
    "####### ",
    "#x",
    "- ",
    "- [x] ",
    "1. ",
    "3. ",
    "> ",
    ">> ",
    ">",
    "| ",
    "@t\n| A | é |\n| ",
    "@dl\nTerm é\n: ",
    ": ",
    "  ",
    "    ",
    "\t",
    "// ",
    "/* ",
    "/*",
    "---",
    "----",
    "```",
    "```go @minify\n",
    "@code\n",
    "@math\n",
    "@callout(kind: note)\n",
    "@details(summary: \"é\")\n",
    "@end",
    "+++",
];

const LINE_END: &[&str] = &[
    "\n", "\n", "\n", "\r\n", "  \n", "\\\n", "\n\n", " \t\n", "",
];

fn gen_doc(rng: &mut XorShift) -> String {
    let mut s = String::new();
    if rng.below(10) == 0 {
        s.push_str("+++\ntitle = \"é\"\nx = [\n+++\n");
    }
    for _ in 0..1 + rng.below(12) {
        s.push_str(rng.pick(LINE_START));
        for _ in 0..rng.below(6) {
            s.push_str(rng.pick(INLINE));
        }
        s.push_str(rng.pick(LINE_END));
    }
    s
}

fn check(src: &str, span: Span, what: &str, errors: &mut Vec<String>) {
    let start = span.start as usize;
    let end = span.end();
    let ok = end <= src.len() && src.is_char_boundary(start) && src.is_char_boundary(end);
    if !ok {
        errors.push(format!("{} span {}..{} in {:?}", what, start, end, src));
    }
}

fn check_inlines(src: &str, seq: &[Inline], errors: &mut Vec<String>) {
    for n in seq {
        match n {
            Inline::Text { span, .. }
            | Inline::InlineCode { span, .. }
            | Inline::HardBreak { span } => check(src, *span, "inline", errors),
            Inline::Bold { content, span }
            | Inline::Italic { content, span }
            | Inline::Underline { content, span }
            | Inline::Strike { content, span } => {
                check(src, *span, "emphasis", errors);
                check_inlines(src, content, errors);
            }
            Inline::Shortcode { content, span, .. } => {
                check(src, *span, "shortcode", errors);
                if let Some(c) = content {
                    check_inlines(src, c, errors);
                }
            }
            // `Inline` is non-exhaustive; new variants need a case above.
            _ => {}
        }
    }
}

fn check_blocks(src: &str, blocks: &[Block], errors: &mut Vec<String>) {
    for b in blocks {
        match b {
            Block::Heading { content, span, .. } | Block::Paragraph { content, span } => {
                check(src, *span, "block", errors);
                check_inlines(src, content, errors);
            }
            Block::List { items, span, .. } => {
                check(src, *span, "list", errors);
                for it in items {
                    check(src, it.span, "list item", errors);
                    check_inlines(src, &it.content, errors);
                    check_blocks(src, &it.children, errors);
                }
            }
            Block::Blockquote { children, span } | Block::BlockShortcode { children, span, .. } => {
                check(src, *span, "container", errors);
                check_blocks(src, children, errors);
            }
            Block::CodeBlock { span, .. } | Block::HorizontalRule { span } => {
                check(src, *span, "leaf block", errors);
            }
            Block::Table {
                header, rows, span, ..
            } => {
                check(src, *span, "table", errors);
                for r in std::iter::once(header).chain(rows) {
                    check(src, r.span, "row", errors);
                    for c in &r.cells {
                        check_inlines(src, c, errors);
                    }
                }
            }
            Block::DefinitionList { items, span, .. } => {
                check(src, *span, "dl", errors);
                for it in items {
                    check(src, it.span, "dl item", errors);
                    check_inlines(src, &it.term, errors);
                    check_inlines(src, &it.definition, errors);
                }
            }
            // `Block` is non-exhaustive; new variants need a case above.
            _ => {}
        }
    }
}

fn check_diags(src: &SourceMap, diags: &[Diagnostic], errors: &mut Vec<String>) {
    for d in diags {
        check(&src.source, d.span, d.code.to_string().as_str(), errors);
    }
    if errors.is_empty() {
        // Rendering slices the source by line and column; it must not panic.
        let _ = diag::render_all(diags, src);
    }
}

#[test]
fn spans_stay_in_bounds_and_on_char_boundaries() {
    let reg = Registry::with_builtins();
    let mut rng = XorShift(0x9e37_79b9_7f4a_7c15);
    let mut errors = Vec::new();
    for _ in 0..4000 {
        let input = gen_doc(&mut rng);
        let src = SourceMap::new("o.brf", input.as_str());
        let tokens = match lexer::lex(&src) {
            Ok(t) => t,
            Err(d) => {
                check_diags(&src, &d, &mut errors);
                continue;
            }
        };
        let (mut doc, mut diags) = parser::parse(tokens, &src);
        diags.extend(resolve::resolve(&mut doc, &reg));
        diags.extend(validate::validate(&doc, &ValidateOptions::default(), &src));
        check_blocks(&src.source, &doc.blocks, &mut errors);
        check_diags(&src, &diags, &mut errors);
        // Emitters must not panic on whatever AST the parser recovered.
        let _ = html::render(&doc, &reg);
        let _ = llm::render(&doc, &reg, &llm::Options::default());
    }
    assert!(
        errors.is_empty(),
        "{} bad spans, first ones:\n{}",
        errors.len(),
        errors[..errors.len().min(10)].join("\n")
    );
}
