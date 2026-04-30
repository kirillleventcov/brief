use brief::diag::{Code, render_all};
use brief::emit::{html, llm};
use brief::lexer::lex;
use brief::parser::parse;
use brief::resolve::resolve;
use brief::shortcode::Registry;
use brief::span::SourceMap;
use brief::validate::{ValidateOpts, validate};

fn compile(input: &str) -> (String, Vec<Code>) {
    let reg = Registry::with_builtins();
    let opts = ValidateOpts::default();
    let src = SourceMap::new("t.brf", input);
    let tokens = match lex(&src) {
        Ok(t) => t,
        Err(d) => return (render_all(&d, &src), d.iter().map(|x| x.code).collect()),
    };
    let (mut doc, mut diags) = parse(tokens, &src);
    diags.extend(resolve(&mut doc, &reg));
    diags.extend(validate(&doc, &opts));
    if !diags.is_empty() {
        return (
            render_all(&diags, &src),
            diags.iter().map(|x| x.code).collect(),
        );
    }
    (html::render(&doc, &reg), vec![])
}

#[test]
fn t_12_1_heading_too_deep() {
    let (_, codes) = compile("# A\n## B\n### C\n####### D\n");
    assert!(codes.contains(&Code::HeadingTooDeep), "{:?}", codes);
}

#[test]
fn t_12_2_out_of_order_ordered() {
    let (_, codes) = compile("1. one\n3. three\n");
    assert!(codes.contains(&Code::OrderedListSequence));
}

#[test]
fn t_12_3_same_marker_emphasis() {
    let (_, codes) = compile("This is *outer *inner* outer*.\n");
    assert!(codes.contains(&Code::EmphasisSameMarker));
}

#[test]
fn t_12_4_snake_case_literal() {
    let (html, codes) = compile("The variable snake_case_name is here.\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(!html.contains("<em>"));
    assert!(html.contains("snake_case_name"));
}

#[test]
fn t_12_5_table_mismatch() {
    let (_, codes) = compile("@t\n| A | B | C\n| 1 | 2\n");
    assert!(codes.contains(&Code::TableColumnMismatch));
}

#[test]
fn t_12_6_unknown_shortcode() {
    let (_, codes) = compile("@frobnicate[hello]\n");
    assert!(codes.contains(&Code::UnknownShortcode));
}

#[test]
fn t_12_7_llm_token_reduction() {
    let brief_src = "## Quarterly Results\n\nThe team delivered *strong* numbers this quarter, with *revenue* up 23%.\n\n@t\n| Region | Q3 | Q4\n| EMEA | 1.2M | 1.5M\n";
    let md_equivalent = "## Quarterly Results\n\nThe team delivered **strong** numbers this quarter, with **revenue** up 23%.\n\n| Region | Q3   | Q4   |\n|--------|------|------|\n| EMEA   | 1.2M | 1.5M |\n";
    let src = SourceMap::new("t.brf", brief_src);
    let tokens = lex(&src).unwrap();
    let (mut doc, diags) = parse(tokens, &src);
    assert!(diags.is_empty(), "{:?}", diags);
    let reg = Registry::with_builtins();
    let r = resolve(&mut doc, &reg);
    assert!(r.is_empty(), "{:?}", r);
    let (llm_out, _w) = llm::render(&doc, &reg, &llm::Opts::default());
    assert!(
        llm_out.chars().count() < md_equivalent.chars().count(),
        "brief llm ({}) should be shorter than markdown ({}):\n--- brief ---\n{}\n--- md ---\n{}",
        llm_out.chars().count(),
        md_equivalent.chars().count(),
        llm_out,
        md_equivalent
    );
}

#[test]
fn html_basic_render() {
    let (html, codes) = compile("# Title\n\nA *bold* and _ital_ paragraph.\n\n- one\n- two\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("<h1>Title</h1>"));
    assert!(html.contains("<strong>bold</strong>"));
    assert!(html.contains("<em>ital</em>"));
    assert!(html.contains("<ul>"));
}

#[test]
fn callout_block_shortcode() {
    let (html, codes) = compile("@callout(kind: warning)\nbe careful\n@end\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("callout-warning"));
    assert!(html.contains("be careful"));
}

#[test]
fn callout_bad_enum() {
    let (_, codes) = compile("@callout(kind: scary)\nbody\n@end\n");
    assert!(codes.contains(&Code::BadEnumValue));
}

#[test]
fn link_shortcode_html() {
    let (html, codes) = compile("see @link[here](https://x.example)\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("href=\"https://x.example\""), "{}", html);
    assert!(html.contains(">here</a>"));
}

#[test]
fn no_inline_html_passthrough() {
    let (html, codes) = compile("<script>alert(1)</script>\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("&lt;script&gt;"), "{}", html);
}

#[test]
fn tabs_rejected() {
    let (_, codes) = compile("hi\tthere\n");
    assert!(codes.contains(&Code::TabCharacter));
}

#[test]
fn unterminated_fence() {
    let (_, codes) = compile("```rust\nfn x() {}\n");
    assert!(codes.contains(&Code::UnterminatedFence));
}

#[test]
fn nested_blockquote() {
    let (html, codes) = compile("> outer\n>> inner\n> outer again\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.matches("<blockquote>").count() >= 2);
}

#[test]
fn comments_stripped() {
    let (html, codes) = compile("// hidden\nvisible text\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(!html.contains("hidden"));
    assert!(html.contains("visible text"));
}

#[test]
fn block_comment_stripped() {
    let (html, codes) = compile("/*\nhidden block\n*/\nvisible\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(!html.contains("hidden block"));
    assert!(html.contains("visible"));
}

#[test]
fn explain_runs() {
    // Sanity: the Code enum knows how to format codes.
    assert_eq!(Code::HeadingTooDeep.as_str(), "B0301");
    assert_eq!(Code::EmphasisSameMarker.as_str(), "B0204");
    assert_eq!(Code::OrderedListSequence.as_str(), "B0501");
}

#[test]
fn ordered_list_starts_at_one() {
    let (_, codes) = compile("2. starts at 2\n");
    assert!(codes.contains(&Code::OrderedListSequence));
}

#[test]
fn unknown_target_in_align() {
    // align array length mismatch
    let (_, codes) = compile("@t(align: [left, right])\n| A | B | C\n| 1 | 2 | 3\n");
    assert!(codes.contains(&Code::AlignArrayLength));
}

#[test]
fn callout_missing_required_arg() {
    let (_, codes) = compile("@callout\nbody\n@end\n");
    assert!(codes.contains(&Code::MissingArg));
}

#[test]
fn underline_and_strike() {
    let (html, codes) = compile("a +under+ b ~strike~ c\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("<u>under</u>"));
    assert!(html.contains("<s>strike</s>"));
}

#[test]
fn doubled_emphasis_not_treated_as_emphasis() {
    let (html, codes) = compile("**not bold**\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(!html.contains("<strong>"), "{}", html);
}

#[test]
fn paragraph_hard_break() {
    let (html, codes) = compile("line one\\\nline two\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("<br>"));
}

#[test]
fn nested_list_with_ordered_inside_unordered() {
    let (html, codes) = compile("- top\n  1. a\n  2. b\n- next\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("<ol>"));
    assert!(html.contains("<ul>"));
}

#[test]
fn stray_end_errors() {
    let (_, codes) = compile("@end\n");
    assert!(codes.contains(&Code::StrayEnd));
}

#[test]
fn unterminated_block_shortcode() {
    let (_, codes) = compile("@callout(kind: info)\nbody\n");
    assert!(codes.contains(&Code::UnterminatedBlock));
}

#[test]
fn footnote_html_auto_numbers() {
    let (html, codes) = compile("First.@footnote[note one]\n\nSecond.@footnote[note two]\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(
        html.contains("href=\"#fn-1\">1</a>"),
        "missing first ref: {}",
        html
    );
    assert!(
        html.contains("href=\"#fn-2\">2</a>"),
        "missing second ref: {}",
        html
    );
    assert!(
        html.contains("<ol class=\"footnotes\">"),
        "missing footnotes section: {}",
        html
    );
    assert!(html.contains("<li id=\"fn-1\">note one"), "{}", html);
    assert!(html.contains("<li id=\"fn-2\">note two"), "{}", html);
    assert!(html.contains("href=\"#fn-ref-1\""), "{}", html);
    assert!(html.contains("href=\"#fn-ref-2\""), "{}", html);
}

#[test]
fn footnote_html_renders_inline_emphasis_in_body() {
    // The footnote body is parsed as inline content, so emphasis markers
    // inside it must reach the rendered <li>.
    let (html, codes) = compile("Claim.@footnote[See _ibid._, p. 5]\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("<em>ibid.</em>"), "{}", html);
}

#[test]
fn footnote_llm_uses_pandoc_style() {
    let brief_src = "First.@footnote[a]\n\nSecond.@footnote[b]\n";
    let src = SourceMap::new("t.brf", brief_src);
    let tokens = lex(&src).unwrap();
    let (mut doc, diags) = parse(tokens, &src);
    assert!(diags.is_empty(), "{:?}", diags);
    let reg = Registry::with_builtins();
    let r = resolve(&mut doc, &reg);
    assert!(r.is_empty(), "{:?}", r);
    let (out, _w) = llm::render(&doc, &reg, &llm::Opts::default());
    assert!(out.contains("First.[^1]"), "{}", out);
    assert!(out.contains("Second.[^2]"), "{}", out);
    assert!(out.contains("[^1]: a"), "{}", out);
    assert!(out.contains("[^2]: b"), "{}", out);
}

#[test]
fn footnote_no_footnotes_no_section() {
    // A document without footnotes must not emit an empty footnotes section.
    let (html, codes) = compile("Plain paragraph.\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(!html.contains("footnotes-sep"), "{}", html);
    assert!(!html.contains("<ol class=\"footnotes\""), "{}", html);
}

#[test]
fn footnote_inside_list_item_numbered_in_document_order() {
    let (html, codes) = compile("- one@footnote[a]\n- two@footnote[b]\n");
    assert!(codes.is_empty(), "{:?}", codes);
    let pos1 = html.find("href=\"#fn-1\">1</a>").expect(&html);
    let pos2 = html.find("href=\"#fn-2\">2</a>").expect(&html);
    assert!(pos1 < pos2, "footnote order wrong: {}", html);
}

#[test]
fn footnote_nested_in_body_not_double_numbered() {
    // A footnote ref inside another footnote body must NOT introduce a new
    // numbered definition; the nested ref renders as plain bracketed text so
    // document-level numbering stays linear.
    let (html, codes) = compile("Claim.@footnote[outer @footnote[inner ignored]]\n");
    assert!(codes.is_empty(), "{:?}", codes);
    // Exactly one <li> in the footnotes list.
    let li_count = html.matches("<li id=\"fn-").count();
    assert_eq!(li_count, 1, "{}", html);
    // Only one auto-numbered ref in the document body.
    let ref_count = html.matches("class=\"fn-ref\"").count();
    assert_eq!(ref_count, 1, "{}", html);
}
