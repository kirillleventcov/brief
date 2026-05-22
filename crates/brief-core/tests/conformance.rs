use brief::diag::{Code, Diagnostic, Severity, render_all};
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
    diags.extend(validate(&doc, &opts, &src));
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

// --- v0.4 §4.2 inline-HTML replacement shortcodes -----------------------

fn render_llm(input: &str) -> String {
    let src = SourceMap::new("t.brf", input);
    let toks = lex(&src).unwrap();
    let (mut doc, diags) = parse(toks, &src);
    assert!(diags.is_empty(), "{:?}", diags);
    let reg = Registry::with_builtins();
    let r = resolve(&mut doc, &reg);
    assert!(r.is_empty(), "{:?}", r);
    let (out, _w) = llm::render(&doc, &reg, &llm::Opts::default());
    out
}

#[test]
fn sub_inline_html() {
    let (html, codes) = compile("water is H@sub[2]O\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("H<sub>2</sub>O"), "{}", html);
}

#[test]
fn sup_inline_html() {
    let (html, codes) = compile("E = mc@sup[2]\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("mc<sup>2</sup>"), "{}", html);
}

#[test]
fn sub_inline_llm() {
    let out = render_llm("water is H@sub[2]O\n");
    assert!(out.contains("H[sub:2]O"), "{}", out);
}

#[test]
fn sup_inline_llm() {
    let out = render_llm("E = mc@sup[2]\n");
    assert!(out.contains("mc[sup:2]"), "{}", out);
}

#[test]
fn details_block_html() {
    let (html, codes) = compile("@details(summary: \"Stack trace\")\nbody line\n@end\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(
        html.contains("<details><summary>Stack trace</summary>"),
        "{}",
        html
    );
    assert!(html.contains("body line"), "{}", html);
    assert!(html.contains("</details>"), "{}", html);
}

#[test]
fn details_block_llm() {
    let out = render_llm("@details(summary: \"Stack trace\")\nbody line\n@end\n");
    assert!(out.contains("[details: \"Stack trace\"]"), "{}", out);
    assert!(out.contains("body line"), "{}", out);
    assert!(out.contains("[/details]"), "{}", out);
}

#[test]
fn details_requires_summary() {
    let (_, codes) = compile("@details\nbody\n@end\n");
    assert!(codes.contains(&Code::MissingArg), "{:?}", codes);
}

#[test]
fn details_inline_form_is_form_mismatch() {
    // `@details` is registered as block-only; using it inline must surface
    // FormMismatch rather than silently degrade.
    let (_, codes) = compile("see @details(summary: \"x\")[body] inline\n");
    assert!(codes.contains(&Code::FormMismatch), "{:?}", codes);
}

#[test]
fn br_shortcode_rejected() {
    // Brief uses `\` for hard breaks. `@br` must not silently accept;
    // surface UnknownShortcode with a help message that points at `\`.
    let (rendered, codes) = compile("line@br\n");
    assert!(codes.contains(&Code::UnknownShortcode), "{:?}", codes);
    assert!(
        rendered.contains("`@br` is not a Brief shortcode"),
        "label missing: {}",
        rendered
    );
    assert!(
        rendered.contains("`\\` at end of line"),
        "help missing: {}",
        rendered
    );
}

#[test]
fn kbd_inline_still_wired() {
    // Sanity: `@kbd` keeps emitting <kbd> in HTML and `[kbd:...]` in LLM.
    let (html, codes) = compile("press @kbd[Ctrl+C]\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("<kbd>Ctrl+C</kbd>"), "{}", html);
    let llm_out = render_llm("press @kbd[Ctrl+C]\n");
    assert!(llm_out.contains("[kbd:Ctrl+C]"), "{}", llm_out);
}

// --- v0.4 §4.3 task lists ----------------------------------------------

#[test]
fn task_list_done_html() {
    let (html, codes) = compile("- [x] ship it\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(
        html.contains("<ul class=\"contains-task-list\">"),
        "parent class missing: {}",
        html
    );
    assert!(
        html.contains(
            "<li class=\"task-list-item\"><input type=\"checkbox\" disabled checked> ship it</li>"
        ),
        "{}",
        html
    );
}

#[test]
fn task_list_todo_html() {
    let (html, codes) = compile("- [ ] write tests\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(
        html.contains(
            "<li class=\"task-list-item\"><input type=\"checkbox\" disabled> write tests</li>"
        ),
        "{}",
        html
    );
}

#[test]
fn task_list_mixed_with_plain_items() {
    let (html, codes) = compile("- [x] one\n- two\n- [ ] three\n");
    assert!(codes.is_empty(), "{:?}", codes);
    // `<ul>` carries the contains-task-list class because at least one
    // item is a task; the plain `- two` keeps a vanilla `<li>`.
    assert!(
        html.contains("<ul class=\"contains-task-list\">"),
        "{}",
        html
    );
    assert!(
        html.contains("<li>two</li>"),
        "plain li lost class: {}",
        html
    );
    assert!(
        html.contains("class=\"task-list-item\"><input type=\"checkbox\" disabled checked>"),
        "{}",
        html
    );
}

#[test]
fn task_list_llm_round_trips_marker() {
    let out = render_llm("- [x] one\n- [ ] two\n");
    assert!(out.contains("- [x] one"), "{}", out);
    assert!(out.contains("- [ ] two"), "{}", out);
}

#[test]
fn task_list_marker_uppercase_x_is_plain_text() {
    // The spec is strict: lowercase `x` only.
    let (html, codes) = compile("- [X] not a task\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(!html.contains("contains-task-list"), "{}", html);
    assert!(!html.contains("task-list-item"), "{}", html);
    assert!(
        html.contains("[X] not a task"),
        "literal preserved: {}",
        html
    );
}

#[test]
fn task_list_marker_double_space_is_plain_text() {
    // `[  ]` (two spaces) is not a task marker.
    let (html, codes) = compile("- [  ] still text\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(!html.contains("contains-task-list"), "{}", html);
    assert!(html.contains("[  ] still text"), "{}", html);
}

#[test]
fn task_list_no_space_after_marker_is_plain_text() {
    // The trailing space after `]` is required to consume the marker.
    let (html, codes) = compile("- [x]nope\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(!html.contains("task-list-item"), "{}", html);
    assert!(html.contains("[x]nope"), "{}", html);
}

#[test]
fn task_list_with_nested_children() {
    // Children render under a task item the same way they do under any list.
    let (html, codes) = compile("- [x] parent\n  - child\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(
        html.contains("class=\"task-list-item\"><input type=\"checkbox\" disabled checked> parent"),
        "{}",
        html
    );
    assert!(html.contains("<ul>\n<li>child</li>"), "nested ul: {}", html);
}

#[test]
fn task_list_inline_emphasis_in_content() {
    // The marker is consumed cleanly, and inline parsing still applies to
    // what follows.
    let (html, codes) = compile("- [ ] ship _eventually_\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("<em>eventually</em>"), "{}", html);
}

#[test]
fn task_list_ordered_done_html() {
    let (html, codes) = compile("1. [x] ship it\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(
        html.contains("<ol class=\"contains-task-list\">"),
        "parent class missing: {}",
        html
    );
    assert!(
        html.contains(
            "<li class=\"task-list-item\"><input type=\"checkbox\" disabled checked> ship it</li>"
        ),
        "{}",
        html
    );
}

#[test]
fn task_list_ordered_todo_html() {
    let (html, codes) = compile("1. [ ] write tests\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(
        html.contains("<ol class=\"contains-task-list\">"),
        "parent class missing: {}",
        html
    );
    assert!(
        html.contains(
            "<li class=\"task-list-item\"><input type=\"checkbox\" disabled> write tests</li>"
        ),
        "{}",
        html
    );
}

#[test]
fn task_list_ordered_llm_round_trips() {
    let out = render_llm("1. [x] one\n2. [ ] two\n");
    assert!(out.contains("1. [x] one"), "{}", out);
    assert!(out.contains("2. [ ] two"), "{}", out);
}

#[test]
fn task_list_ordered_marker_uppercase_x_is_plain_text() {
    // The spec is strict: lowercase `x` only.
    let (html, codes) = compile("1. [X] not a task\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(!html.contains("contains-task-list"), "{}", html);
    assert!(!html.contains("task-list-item"), "{}", html);
    assert!(
        html.contains("[X] not a task"),
        "literal preserved: {}",
        html
    );
}

#[test]
fn task_list_ordered_no_space_after_marker_is_plain_text() {
    // The trailing space after `]` is required to consume the marker.
    let (html, codes) = compile("1. [x]nope\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(!html.contains("task-list-item"), "{}", html);
    assert!(html.contains("[x]nope"), "{}", html);
}

#[test]
fn task_list_ordered_double_space_is_plain_text() {
    // `[  ]` (two spaces) is not a task marker.
    let (html, codes) = compile("1. [  ] still text\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(!html.contains("contains-task-list"), "{}", html);
    assert!(html.contains("[  ] still text"), "{}", html);
}

// --- 4.4 @link title: kwarg and @callout GFM kinds ----------------------

/// Helper that returns (html, all_diags) without short-circuiting on warnings.
fn compile_with_diags(input: &str) -> (String, Vec<Diagnostic>) {
    let reg = Registry::with_builtins();
    let opts = ValidateOpts::default();
    let src = SourceMap::new("t.brf", input);
    let tokens = match lex(&src) {
        Ok(t) => t,
        Err(d) => return (render_all(&d, &src), d),
    };
    let (mut doc, mut diags) = parse(tokens, &src);
    diags.extend(resolve(&mut doc, &reg));
    diags.extend(validate(&doc, &opts, &src));
    let has_errors = diags.iter().any(|d| d.severity == Severity::Error);
    if has_errors {
        return (render_all(&diags, &src), diags);
    }
    (html::render(&doc, &reg), diags)
}

// --- link title: kwarg ---

#[test]
fn link_title_html() {
    let (html, codes) = compile("see @link(title: \"alt text\")[here](https://x.example)\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("href=\"https://x.example\""), "{}", html);
    assert!(html.contains("title=\"alt text\""), "{}", html);
    assert!(html.contains(">here</a>"), "{}", html);
}

#[test]
fn link_title_llm() {
    let src = SourceMap::new(
        "t.brf",
        "see @link(title: \"alt text\")[here](https://x.example)\n",
    );
    let toks = lex(&src).unwrap();
    let (mut doc, diags) = parse(toks, &src);
    assert!(diags.is_empty(), "{:?}", diags);
    let reg = Registry::with_builtins();
    let r = resolve(&mut doc, &reg);
    assert!(r.is_empty(), "{:?}", r);
    let (out, _w) = llm::render(&doc, &reg, &llm::Opts::default());
    assert!(
        out.contains("[here](https://x.example \"alt text\")"),
        "{}",
        out
    );
}

#[test]
fn link_no_title_html() {
    // Without title, the old format must be used.
    let (html, codes) = compile("see @link[here](https://x.example)\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("href=\"https://x.example\""), "{}", html);
    assert!(!html.contains("title="), "{}", html);
}

// --- @callout new GFM kinds ---

#[test]
fn callout_kind_note() {
    let (html, codes) = compile("@callout(kind: note)\nbody\n@end\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("callout-note"), "{}", html);
}

#[test]
fn callout_kind_tip() {
    let (html, codes) = compile("@callout(kind: tip)\nbody\n@end\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("callout-tip"), "{}", html);
}

#[test]
fn callout_kind_important() {
    let (html, codes) = compile("@callout(kind: important)\nbody\n@end\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("callout-important"), "{}", html);
}

#[test]
fn callout_kind_caution() {
    let (html, codes) = compile("@callout(kind: caution)\nbody\n@end\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("callout-caution"), "{}", html);
}

#[test]
fn callout_kind_warning_still_valid() {
    let (html, codes) = compile("@callout(kind: warning)\nbody\n@end\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("callout-warning"), "{}", html);
}

#[test]
fn callout_kind_bogus_still_errors() {
    let (_, codes) = compile("@callout(kind: bogus)\nbody\n@end\n");
    assert!(codes.contains(&Code::BadEnumValue), "{:?}", codes);
}

// --- @callout deprecation aliases ---

#[test]
fn callout_kind_info_deprecated_emits_warning_and_rewrites() {
    let (html, diags) = compile_with_diags("@callout(kind: info)\nbody\n@end\n");
    // Must produce a B0408 warning with Severity::Warning
    let warn = diags.iter().find(|d| d.code == Code::DeprecatedCalloutKind);
    assert!(
        warn.is_some(),
        "expected DeprecatedCalloutKind warning: {:?}",
        diags
    );
    assert_eq!(warn.unwrap().severity, Severity::Warning);
    // HTML output must show callout-note (rewritten from info → note)
    assert!(
        html.contains("callout-note"),
        "expected callout-note: {}",
        html
    );
    assert!(
        !html.contains("callout-info"),
        "must not contain callout-info: {}",
        html
    );
}

#[test]
fn callout_kind_info_llm_shows_note() {
    let src = SourceMap::new("t.brf", "@callout(kind: info)\nbody\n@end\n");
    let toks = lex(&src).unwrap();
    let (mut doc, diags) = parse(toks, &src);
    assert!(diags.is_empty(), "{:?}", diags);
    let reg = Registry::with_builtins();
    let r = resolve(&mut doc, &reg);
    // Should have exactly one warning (DeprecatedCalloutKind)
    assert_eq!(r.len(), 1, "expected one warning: {:?}", r);
    assert_eq!(r[0].code, Code::DeprecatedCalloutKind);
    assert_eq!(r[0].severity, Severity::Warning);
    let (out, _w) = llm::render(&doc, &reg, &llm::Opts::default());
    assert!(out.contains("[!note]"), "expected [!note]: {}", out);
}

#[test]
fn callout_kind_danger_deprecated_emits_warning_and_rewrites_to_caution() {
    let (html, diags) = compile_with_diags("@callout(kind: danger)\nbody\n@end\n");
    let warn = diags.iter().find(|d| d.code == Code::DeprecatedCalloutKind);
    assert!(
        warn.is_some(),
        "expected DeprecatedCalloutKind warning: {:?}",
        diags
    );
    assert_eq!(warn.unwrap().severity, Severity::Warning);
    assert!(
        html.contains("callout-caution"),
        "expected callout-caution: {}",
        html
    );
}

#[test]
fn callout_deprecated_kind_warning_in_resolve() {
    // Unit test: resolve() returns the warning Diagnostic with Severity::Warning.
    let src = SourceMap::new("t.brf", "@callout(kind: info)\nbody\n@end\n");
    let toks = lex(&src).unwrap();
    let (mut doc, _parse_diags) = parse(toks, &src);
    let reg = Registry::with_builtins();
    let diags = resolve(&mut doc, &reg);
    let warn = diags.iter().find(|d| d.code == Code::DeprecatedCalloutKind);
    assert!(
        warn.is_some(),
        "resolve must return DeprecatedCalloutKind: {:?}",
        diags
    );
    assert_eq!(warn.unwrap().severity, Severity::Warning);
}

// --- 4.5 Heading anchors ---------------------------------------------------

fn parse_doc(input: &str) -> (brief::ast::Document, Vec<brief::Diagnostic>) {
    use brief::lexer::lex;
    use brief::parser::parse;
    let src = SourceMap::new("t.brf", input);
    let toks = lex(&src).unwrap();
    parse(toks, &src)
}

/// Test 1: valid anchor — AST, HTML, LLM.
#[test]
fn heading_anchor_valid_ast_html_llm() {
    // AST: anchor == Some("hello")
    let (doc, diags) = parse_doc("## Heading {#hello}\n");
    assert!(diags.is_empty(), "{:?}", diags);
    assert_eq!(doc.blocks.len(), 1);
    if let brief::ast::Block::Heading { anchor, .. } = &doc.blocks[0] {
        assert_eq!(
            anchor.as_deref(),
            Some("hello"),
            "anchor should be Some(\"hello\")"
        );
    } else {
        panic!("expected Heading block");
    }

    // HTML: id="hello" on <h2>
    let (html, codes) = compile("## Heading {#hello}\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(
        html.contains("<h2 id=\"hello\">Heading</h2>"),
        "html: {}",
        html
    );

    // LLM: no anchor, just `## Heading\n`
    let llm_out = render_llm("## Heading {#hello}\n");
    assert!(llm_out.contains("## Heading"), "llm: {}", llm_out);
    assert!(
        !llm_out.contains("{#hello}"),
        "anchor must be stripped in llm: {}",
        llm_out
    );
}

/// Test 2: plain heading — no anchor.
#[test]
fn heading_no_anchor() {
    let (doc, diags) = parse_doc("## Plain heading\n");
    assert!(diags.is_empty(), "{:?}", diags);
    if let brief::ast::Block::Heading { anchor, .. } = &doc.blocks[0] {
        assert!(anchor.is_none(), "anchor should be None");
    } else {
        panic!("expected Heading block");
    }

    let (html, codes) = compile("## Plain heading\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("<h2>Plain heading</h2>"), "html: {}", html);
    assert!(!html.contains("id="), "no id attr: {}", html);

    let llm_out = render_llm("## Plain heading\n");
    assert!(llm_out.contains("## Plain heading"), "llm: {}", llm_out);
}

/// Test 3: bad anchor (uppercase + underscore) → B0317.
#[test]
fn heading_anchor_bad_name_uppercase() {
    let (_, codes) = compile("## Bad {#Bad_Anchor}\n");
    assert!(codes.contains(&Code::BadHeadingAnchor), "{:?}", codes);
}

/// Test 4: empty anchor name → B0317.
#[test]
fn heading_anchor_empty_name() {
    let (_, codes) = compile("## Bad {#}\n");
    assert!(codes.contains(&Code::BadHeadingAnchor), "{:?}", codes);
}

/// Test 5: content after `}` → B0317.
#[test]
fn heading_anchor_content_after_brace() {
    let (_, codes) = compile("## Bad {#abc} extra\n");
    assert!(codes.contains(&Code::BadHeadingAnchor), "{:?}", codes);
}

/// Test 6: two spaces before `{` → B0317.
#[test]
fn heading_anchor_double_space_before_brace() {
    let (_, codes) = compile("## Bad  {#abc}\n");
    assert!(codes.contains(&Code::BadHeadingAnchor), "{:?}", codes);
}

/// Test 7: duplicate anchors → B0506.
#[test]
fn heading_anchor_duplicate() {
    let (_, codes) = compile("## A {#x}\n## B {#x}\n");
    assert!(codes.contains(&Code::DuplicateHeadingAnchor), "{:?}", codes);
}

/// Test 8: `{curly}` without `#` — plain text, no diagnostic.
#[test]
fn heading_curly_without_hash_is_plain_text() {
    let (html, codes) = compile("## Heading with {curly} not anchor\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("{curly}"), "curly braces preserved: {}", html);
    assert!(!html.contains("id="), "no id attr: {}", html);
}

/// Test 9: hyphens and digits in anchor name are valid.
#[test]
fn heading_anchor_valid_hyphens_and_digits() {
    let (html, codes) = compile("## Heading {#valid-name-123}\n");
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("id=\"valid-name-123\""), "html: {}", html);
}

#[test]
fn t_definition_list_basic() {
    let input = "@dl\nApple\n: A pomaceous fruit.\nBrief\n: A markup language.\n@end\n";
    let (html, codes) = compile(input);
    assert!(codes.is_empty(), "{:?}", codes);
    assert!(html.contains("<dl>"), "html: {}", html);
    assert!(html.contains("<dt>Apple</dt>"), "html: {}", html);
    assert!(
        html.contains("<dd>A pomaceous fruit.</dd>"),
        "html: {}",
        html
    );
    assert!(html.contains("<dt>Brief</dt>"), "html: {}", html);
    assert!(
        html.contains("<dd>A markup language.</dd>"),
        "html: {}",
        html
    );
}

#[test]
fn t_definition_list_malformed_is_b0505() {
    let (_, codes) = compile("@dl\n: Stray definition.\n@end\n");
    assert!(codes.contains(&Code::BadDefinitionList), "{:?}", codes);
}
