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
use std::path::Path;

/// What the compiler front-end makes of `source`: the span-stripped AST
/// and the diagnostic codes, in order. `None` if the source does not lex.
fn parse_summary(source: &str) -> Option<(String, Vec<brief::Code>)> {
    // Every tool reads sources through `read_source`, which drops a BOM.
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let src = SourceMap::new("t.brf", source.to_string());
    let toks = lexer::lex(&src).ok()?;
    let (doc, diags) = parser::parse(toks, &src);
    // Tabs and stray BOMs outside verbatim regions are what fmt exists to
    // repair, so fixing them is allowed to change the parse.
    if diags.iter().any(|d| {
        matches!(
            d.code,
            brief::Code::TabCharacter | brief::Code::BomNotAtStart
        )
    }) {
        return None;
    }
    let codes = diags.iter().map(|d| d.code).collect();
    Some((strip_spans(&format!("{:#?}", doc)), codes))
}

fn parse_doc_string(source: &str) -> String {
    let (doc, codes) = parse_summary(source).expect("lex must succeed for fixture inputs");
    assert!(
        codes.is_empty(),
        "fixture parsed with diagnostics: {:?}\nsrc: {:?}",
        codes,
        source
    );
    doc
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
            // Copy the whole UTF-8 sequence, not a single byte.
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
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

/// The full contract for any input, valid or not: formatting is
/// idempotent, and when the input lexes, the formatted source parses to
/// the same AST with the same diagnostics.
fn assert_fmt_contract(label: &str, src: &str) {
    let opts = brief::fmt::Opts::default();
    let once = brief::fmt::format(src, &opts);
    let twice = brief::fmt::format(&once, &opts);
    assert_eq!(once, twice, "[{}] format() is not idempotent", label);
    let Some(before) = parse_summary(src) else {
        return;
    };
    let after = parse_summary(&once)
        .unwrap_or_else(|| panic!("[{}] formatted source no longer lexes:\n{}", label, once));
    assert_eq!(
        before, after,
        "[{}] format() changed what the parser sees\n--- raw ---\n{}\n--- formatted ---\n{}",
        label, src, once
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
fn semantic_preserved_table_escaped_pipe() {
    // FMT-01: this used to come back as a B0502 column mismatch.
    assert_semantic_preserved("escaped_pipe", "@t\n| a \\| b | c\n| d | e\n");
}

#[test]
fn semantic_preserved_table_pipe_in_code_span() {
    assert_semantic_preserved("code_span_pipe", "@t\n| `a|b` | c\n| d | e\n");
    assert_semantic_preserved("double_tick_pipe", "@t\n| ``a|b`` | c\n| d | e\n");
}

#[test]
fn semantic_preserved_table_empty_last_cell() {
    assert_semantic_preserved("empty_last_cell", "@t\n| a |  |\n| bb | c\n");
}

#[test]
fn semantic_preserved_code_shortcode_body() {
    // FMT-02: blank runs and tables inside `@code` are code.
    assert_semantic_preserved(
        "code_shortcode",
        "@code(brief)\n@t\n| a | b\n| longer | c\n\n\n\nx   \n@end\n",
    );
}

#[test]
fn semantic_preserved_lone_fence_inside_code_shortcode() {
    assert_semantic_preserved(
        "lone_fence",
        "@code\n```\n@end\n\n\n\npara   \n\n```\nkeep\n\n\n```\n",
    );
}

#[test]
fn contract_holds_for_adversarial_inputs() {
    let cases = [
        ("unterminated_fence", "para\n```\ncode   \n\n\n"),
        ("unterminated_code_shortcode", "@code\nx\n\n\n"),
        (
            "unterminated_frontmatter",
            "+++\nx = 1\n\n\n# not a heading\n",
        ),
        ("plus_after_blank", "\n\n+++\nx = 1\n+++\n"),
        (
            "fence_in_comment",
            "/*\n```\n*/\npara   \n\n\n```\nx\n\n\n```\n",
        ),
        ("fence_in_list", "- item\n\n  ```\n  a\n\n\n  ```\n- next\n"),
        ("end_at_other_indent", "@code\n  @end\n\n\nx\n@end\n"),
        (
            "table_in_shortcode",
            "@callout(kind: info)\n@t\n| a | bb\n| ccc | d\n@end\n",
        ),
        (
            "unclosed_backtick_row",
            "@t\n| a | b\n| `x | y\n| longer | c\n",
        ),
        ("column_mismatch", "@t\n| a | b\n| c\n| d | e | f\n"),
        ("bare_cr", "# a\rb\r\nbody\r\n"),
        ("bom", "\u{feff}+++\nx = 1\n+++\n# Doc\n"),
        ("tab_in_code", "```\n\tx\n```\n"),
        ("tab_indent", "- a\n\t- b\n"),
        ("trailing_ws_in_fence", "```\nx   \n\n\n```\n"),
    ];
    for (label, src) in cases {
        assert_fmt_contract(label, src);
    }
}

#[test]
fn contract_holds_for_every_repo_source() {
    // Every `.brf` in the workspace: the docs, the examples,
    // LearnXinYminutes, test fixtures and the fuzz corpus.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let files = brief::project::brf_files(&root);
    assert!(
        files.len() > 20,
        "expected the repo corpus, found {} files",
        files.len()
    );
    for path in files {
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_fmt_contract(&path.display().to_string(), &raw);
    }
}

#[test]
fn semantic_preserved_for_real_repo_doc() {
    // The flagship corpus file. If this passes, the formatter is
    // semantically transparent for representative real inputs.
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("LearnXinYminutes.brf");
    let raw = std::fs::read_to_string(&p).unwrap();
    assert_semantic_preserved("LearnXinYminutes.brf", &raw);
}
