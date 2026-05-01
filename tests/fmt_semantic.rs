//! Semantic-preservation tests for `brief::fmt`.
//!
//! The formatter is allowed to change source bytes, but it must NOT change
//! the document the compiler parses out of those bytes. We verify by
//! parsing both the raw and the formatted source through the real lexer +
//! parser and comparing the resulting `Document` debug printouts (the AST
//! holds spans which differ across formatting passes — debug prints
//! normalise that for the comparison only at the structural level we care
//! about, since spans are byte offsets that of course shift).
//!
//! To make the comparison span-agnostic we strip span debug fragments
//! before diffing. This is brittle if `Span`'s `Debug` output ever changes,
//! but it's a focused enough test to be worth that fragility.

use brief::lexer;
use brief::parser;
use brief::span::SourceMap;

fn parse_doc_string(source: &str) -> String {
    let src = SourceMap::new("t.brf", source.to_string());
    let toks = lexer::lex(&src).expect("lex must succeed for fixture inputs");
    let (doc, diags) = parser::parse(toks, &src);
    let errs: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == brief::diag::Severity::Error)
        .collect();
    assert!(
        errs.is_empty(),
        "fixture parsed with errors: {:?}\nsrc: {:?}",
        errs,
        source
    );
    let dbg = format!("{:#?}", doc);
    strip_spans(&dbg)
}

/// Remove `Span { start: N, len: N }` segments from a debug dump so two
/// otherwise-equivalent ASTs at different byte offsets compare equal.
fn strip_spans(s: &str) -> String {
    // Cheap state machine: scan for `Span {` and skip until the matching
    // `}`. No nested braces inside a Span debug.
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"Span {") {
            // Skip until next `}`.
            let mut j = i + 6;
            while j < bytes.len() && bytes[j] != b'}' {
                j += 1;
            }
            // Consume the `}` itself if found.
            if j < bytes.len() {
                j += 1;
            }
            out.push_str("Span{}");
            i = j;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn assert_semantic_preserved(label: &str, src: &str) {
    let opts = brief::fmt::Opts::default();
    let formatted = brief::fmt::format(src, &opts);
    let raw_doc = parse_doc_string(src);
    let fmt_doc = parse_doc_string(&formatted);
    assert_eq!(
        raw_doc, fmt_doc,
        "[{}] format() changed parsed AST\n--- raw ---\n{}\n--- formatted ---\n{}",
        label, src, formatted
    );
}

#[test]
fn semantic_preserved_simple_paragraph() {
    assert_semantic_preserved("paragraph", "hello world\n");
}

#[test]
fn semantic_preserved_heading() {
    assert_semantic_preserved("heading", "# Title\n\nbody\n");
}

#[test]
fn semantic_preserved_emphasis_marker_choice() {
    // Emphasis marker selection must survive — the formatter must not
    // canonicalize `_x_` to `*x*` or vice versa.
    assert_semantic_preserved("emph_underscore", "hello _world_\n");
    assert_semantic_preserved("emph_star", "hello *world*\n");
    assert_semantic_preserved("emph_strike", "hello ~strike~\n");
}

#[test]
fn semantic_preserved_lists() {
    assert_semantic_preserved("nested_lists", "- a\n  - a1\n  - a2\n- b\n");
    assert_semantic_preserved("ordered", "1. one\n2. two\n3. three\n");
}

#[test]
fn semantic_preserved_table() {
    assert_semantic_preserved(
        "ragged_table",
        "@t\n| Header | B\n| longcell | y\n| z | other\n",
    );
}

#[test]
fn semantic_preserved_code_fence_with_attrs() {
    assert_semantic_preserved(
        "fence_attrs",
        "```rust @minify-keep-comments\n  fn x() {}\n```\n",
    );
}

#[test]
fn semantic_preserved_block_shortcode() {
    assert_semantic_preserved("callout", "@callout(kind: warning)\nbody text\n@end\n");
}

#[test]
fn semantic_preserved_inline_shortcode() {
    assert_semantic_preserved(
        "link",
        "see @link(href: \"https://example.com\", title: \"Ex\")\n",
    );
}

#[test]
fn semantic_preserved_block_comment() {
    assert_semantic_preserved("comment", "/*\n  hello\n*/\n# heading\n");
}

#[test]
fn semantic_preserved_dirty_input() {
    // Input has trailing whitespace, extra blank lines, mixed CRLF.
    let src = "  # heading   \r\n\r\n\r\n\r\nbody one\r\n\r\nbody two   \r\n";
    assert_semantic_preserved("dirty", src);
}

#[test]
fn semantic_preserved_frontmatter_passthrough() {
    let src = "+++\nz = 1\na = 2\n+++\n# Doc\n";
    assert_semantic_preserved("frontmatter", src);
}

#[test]
fn semantic_preserved_for_real_repo_doc() {
    // The flagship corpus file. If this passes, the formatter is
    // semantically transparent for representative real inputs.
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("LearnXinYminutes.brf");
    let raw = std::fs::read_to_string(&p).unwrap();
    assert_semantic_preserved("LearnXinYminutes.brf", &raw);
}
