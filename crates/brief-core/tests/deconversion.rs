//! Brief-to-Markdown converter unit tests.
//!
//! Mirror of `conversion.rs`: each test asserts (a) the exact Markdown
//! produced and (b) the set of `MdHole` codes raised. Order of holes within
//! a single conversion is not asserted unless explicitly stated.

use brief::convert::{MdHole, to_markdown};
use brief::lexer::lex;
use brief::parser::parse;
use brief::resolve::resolve;
use brief::shortcode::{Registry, ShortKindOpt, Shortcode};
use brief::span::SourceMap;

fn run_with(brf: &str, reg: &Registry) -> (String, Vec<MdHole>) {
    let src = SourceMap::new("test.brf", brf);
    let tokens = lex(&src).expect("lex must succeed");
    let (mut doc, parse_diags) = parse(tokens, &src);
    let parse_errors: Vec<_> = parse_diags
        .iter()
        .filter(|d| d.severity == brief::diag::Severity::Error)
        .collect();
    assert!(parse_errors.is_empty(), "parse errors: {:?}", parse_errors);
    // Resolve normalizes positional args into keyword slots. `@ref` errors
    // with B0604 outside a project; to_markdown falls back to parsing the
    // raw target, which is exactly the path these tests exercise.
    let _ = resolve(&mut doc, reg);
    let r = to_markdown(&doc, reg, &src);
    (r.markdown, r.diagnostics.iter().map(|d| d.hole).collect())
}

fn run(brf: &str) -> (String, Vec<MdHole>) {
    run_with(brf, &Registry::with_builtins())
}

// ---------------------------------------------------------------- paragraphs

#[test]
fn plain_paragraph() {
    let (out, holes) = run("hello world\n");
    assert_eq!(out, "hello world\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn two_paragraphs() {
    let (out, holes) = run("first\n\nsecond\n");
    assert_eq!(out, "first\n\nsecond\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn hard_break_preserved() {
    let (out, holes) = run("line one\\\nline two\n");
    assert_eq!(out, "line one\\\nline two\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

// ------------------------------------------------------------------ headings

#[test]
fn heading_levels() {
    let (out, holes) = run("# One\n\n###### Six\n");
    assert_eq!(out, "# One\n\n###### Six\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn heading_anchor_preserved() {
    let (out, holes) = run("## Install {#install}\n");
    assert_eq!(out, "## Install {#install}\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

// ------------------------------------------------------------------ emphasis

#[test]
fn bold_doubled() {
    let (out, holes) = run("a *b* c\n");
    assert_eq!(out, "a **b** c\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn italic_clean() {
    let (out, holes) = run("a _i_ b\n");
    assert_eq!(out, "a _i_ b\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn strike_doubled() {
    let (out, holes) = run("a ~s~ b\n");
    assert_eq!(out, "a ~~s~~ b\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn underline_becomes_html_u() {
    let (out, holes) = run("a +u+ b\n");
    assert_eq!(out, "a <u>u</u> b\n");
    assert_eq!(holes, vec![MdHole::Underline]);
}

#[test]
fn nested_emphasis() {
    let (out, holes) = run("*bold _italic_*\n");
    assert_eq!(out, "**bold _italic_**\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn escaped_sigils_reescaped_for_markdown() {
    // Brief `\*` parses to a literal `*`; Markdown needs it escaped again
    // so it does not open emphasis on the way back.
    let (out, holes) = run("literal \\*star\\* here\n");
    assert_eq!(out, "literal \\*star\\* here\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn inline_code_clean() {
    let (out, holes) = run("run `cargo test` now\n");
    assert_eq!(out, "run `cargo test` now\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

// ---------------------------------------------------------------- shortcodes

#[test]
fn link_clean() {
    let (out, holes) = run("see @link[the spec](https://example.com)\n");
    assert_eq!(out, "see [the spec](https://example.com)\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn link_title_preserved() {
    let (out, holes) = run("@link(title: \"some title\")[t](https://x)\n");
    assert_eq!(out, "[t](https://x \"some title\")\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn image_clean() {
    let (out, holes) = run("@image(src: \"cat.png\", alt: \"A cat\")[]\n");
    assert_eq!(out, "![A cat](cat.png)\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn image_empty_alt() {
    let (out, holes) = run("@image(src: \"cat.png\")[]\n");
    assert_eq!(out, "![](cat.png)\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn kbd_sub_sup_as_inline_html() {
    let (out, holes) = run("press @kbd[Ctrl] for H@sub[2]O and x@sup[2]\n");
    assert_eq!(
        out,
        "press <kbd>Ctrl</kbd> for H<sub>2</sub>O and x<sup>2</sup>\n"
    );
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn inline_math_dollar() {
    let (out, holes) = run("so @math[x^2] holds\n");
    assert_eq!(out, "so $x^2$ holds\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn block_math_double_dollar() {
    let (out, holes) = run("@math\nE = mc^2\n@end\n");
    assert_eq!(out, "$$\nE = mc^2\n$$\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn footnote_hoisted_to_definition() {
    let (out, holes) = run("fact@footnote[the source] stated\n");
    assert_eq!(out, "fact[^1] stated\n\n[^1]: the source\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn footnotes_numbered_in_document_order() {
    let (out, holes) = run("a@footnote[one]\n\nb@footnote[two]\n");
    assert_eq!(out, "a[^1]\n\nb[^2]\n\n[^1]: one\n[^2]: two\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn ref_becomes_relative_md_link() {
    let (out, holes) = run("see @ref[guide/install.brf#setup](Install)\n");
    assert_eq!(out, "see [Install](guide/install.md#setup)\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn ref_whole_file() {
    let (out, holes) = run("@ref[intro.brf](Intro)\n");
    assert_eq!(out, "[Intro](intro.md)\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn details_becomes_html() {
    let (out, holes) = run("@details(summary: \"More\")\nHidden.\n@end\n");
    assert_eq!(
        out,
        "<details><summary>More</summary>\n\nHidden.\n\n</details>\n"
    );
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn callout_becomes_gfm_alert() {
    let (out, holes) = run("@callout(kind: \"note\")\nCareful now.\n@end\n");
    assert_eq!(out, "> [!NOTE]\n> Careful now.\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn custom_shortcode_falls_back_to_html_template() {
    let mut reg = Registry::with_builtins();
    reg.map.insert(
        "wave".into(),
        Shortcode {
            kind: ShortKindOpt::Inline,
            template_html: Some("<span class=\"wave\">{{content}}</span>".into()),
            ..Default::default()
        },
    );
    let (out, holes) = run_with("hi @wave[there]\n", &reg);
    assert_eq!(out, "hi <span class=\"wave\">there</span>\n");
    assert_eq!(holes, vec![MdHole::CustomShortcode]);
}

#[test]
fn custom_block_shortcode_without_template_unwraps_children() {
    let mut reg = Registry::with_builtins();
    reg.map.insert(
        "box".into(),
        Shortcode {
            kind: ShortKindOpt::Block,
            ..Default::default()
        },
    );
    let (out, holes) = run_with("@box\ninner text\n@end\n", &reg);
    assert_eq!(out, "inner text\n");
    assert_eq!(holes, vec![MdHole::CustomShortcode]);
}

// --------------------------------------------------------------------- lists

#[test]
fn unordered_list() {
    let (out, holes) = run("- a\n- b\n");
    assert_eq!(out, "- a\n- b\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn ordered_list() {
    let (out, holes) = run("1. a\n2. b\n");
    assert_eq!(out, "1. a\n2. b\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn task_list() {
    let (out, holes) = run("- [x] done\n- [ ] todo\n");
    assert_eq!(out, "- [x] done\n- [ ] todo\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn nested_unordered_list() {
    let (out, holes) = run("- a\n  - b\n");
    assert_eq!(out, "- a\n  - b\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn nested_ordered_list_indents_to_marker_width() {
    let (out, holes) = run("1. a\n  1. b\n");
    assert_eq!(out, "1. a\n   1. b\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn list_item_with_paragraph_child() {
    let (out, holes) = run("- a\n  child para\n");
    assert_eq!(out, "- a\n\n  child para\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

// --------------------------------------------------------------- blockquotes

#[test]
fn blockquote_simple() {
    let (out, holes) = run("> quoted\n");
    assert_eq!(out, "> quoted\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn blockquote_two_paragraphs() {
    let (out, holes) = run("> one\n\n> two\n");
    assert_eq!(out, "> one\n\n> two\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

// -------------------------------------------------------------------- tables

#[test]
fn table_no_align_gets_plain_rule() {
    let (out, holes) = run("@t\n| Name | Age\n| Ada | 30\n");
    assert_eq!(out, "| Name | Age |\n| --- | --- |\n| Ada | 30 |\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn table_alignment_rule() {
    let (out, holes) = run("@t(align: [left, center, right])\n| L | C | R\n| a | b | c\n");
    assert_eq!(
        out,
        "| L | C | R |\n| :--- | :---: | ---: |\n| a | b | c |\n"
    );
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn table_cell_pipe_reescaped() {
    let (out, holes) = run("@t\n| A \\| B | C\n| x | y\n");
    assert_eq!(out, "| A \\| B | C |\n| --- | --- |\n| x | y |\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

// ---------------------------------------------------------- definition lists

#[test]
fn definition_list() {
    let (out, holes) = run("@dl\nTerm\n: Definition.\n@end\n");
    assert_eq!(out, "Term\n: Definition.\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn definition_list_two_items() {
    let (out, holes) = run("@dl\nA\n: one.\nB\n: two.\n@end\n");
    assert_eq!(out, "A\n: one.\n\nB\n: two.\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

// -------------------------------------------------------------- other blocks

#[test]
fn horizontal_rule() {
    let (out, holes) = run("a\n\n---\n\nb\n");
    assert_eq!(out, "a\n\n---\n\nb\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn code_fence_with_lang() {
    let (out, holes) = run("```rust\nfn main() {}\n```\n");
    assert_eq!(out, "```rust\nfn main() {}\n```\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn code_fence_attrs_dropped() {
    let (out, holes) = run("```rust @nominify\nlet x = 1;\n```\n");
    assert_eq!(out, "```rust\nlet x = 1;\n```\n");
    assert_eq!(holes, vec![MdHole::CodeAttrs]);
}

#[test]
fn code_body_not_markdown_escaped() {
    let (out, holes) = run("```\na * b _ c\n```\n");
    assert_eq!(out, "```\na * b _ c\n```\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

#[test]
fn frontmatter_toml_passes_through() {
    let (out, holes) = run("+++\ntitle = \"Doc\"\n+++\n\n# Hi\n");
    assert_eq!(out, "+++\ntitle = \"Doc\"\n+++\n\n# Hi\n");
    assert!(holes.is_empty(), "{:?}", holes);
}

// ------------------------------------------------------------------ comments

#[test]
fn line_comment_dropped_with_hole() {
    let (out, holes) = run("// a comment\n\nreal text\n");
    assert_eq!(out, "real text\n");
    assert_eq!(holes, vec![MdHole::Comment]);
}

#[test]
fn block_comment_dropped_with_hole() {
    let (out, holes) = run("/* hidden\nstill hidden */\n\ntext\n");
    assert_eq!(out, "text\n");
    assert_eq!(holes, vec![MdHole::Comment]);
}

#[test]
fn double_slash_inside_fence_is_not_a_comment() {
    let (out, holes) = run("```\n// not a comment\n```\n");
    assert_eq!(out, "```\n// not a comment\n```\n");
    assert!(holes.is_empty(), "{:?}", holes);
}
