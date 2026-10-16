use crate::ast::{Block, Document, Inline};
use crate::diag::{Code, Diagnostic};
use crate::slug::Heading;
use crate::span::{SourceMap, Span};
use std::collections::{BTreeMap, BTreeSet};

pub struct ValidateOpts {
    pub strict_heading_levels: bool,
}

impl Default for ValidateOpts {
    fn default() -> Self {
        ValidateOpts {
            strict_heading_levels: false,
        }
    }
}

pub fn validate(doc: &Document, opts: &ValidateOpts, src: &SourceMap) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    if opts.strict_heading_levels {
        check_heading_monotonic(doc, &mut diags);
    }
    let headings = crate::slug::headings(doc);
    check_duplicate_ids(&headings, src, &mut diags);
    check_fragment_links(&headings, doc, &mut diags);
    diags
}

fn check_heading_monotonic(doc: &Document, diags: &mut Vec<Diagnostic>) {
    let mut last: u8 = 0;
    for b in &doc.blocks {
        if let Block::Heading { level, span, .. } = b {
            if last > 0 && *level > last + 1 {
                diags.push(
                    Diagnostic::new(Code::HeadingMonotonic, *span)
                        .label(format!("heading level jumps from {} to {}", last, level))
                        .help("increment heading levels by at most one"),
                );
            }
            last = *level;
        }
    }
}

/// Two headings with the same effective id would both be emitted under it
/// and every link to it would land on the first, so a duplicate is an error
/// rather than GitHub's silent `-1` suffix.
fn check_duplicate_ids(headings: &[Heading], src: &SourceMap, diags: &mut Vec<Diagnostic>) {
    let mut seen: BTreeMap<&str, Span> = BTreeMap::new();
    for h in headings {
        if let Some(first_span) = seen.get(h.id.as_str()) {
            let (first_line, _) = src.line_col(first_span.start);
            diags.push(
                Diagnostic::new(Code::DuplicateHeadingAnchor, h.span)
                    .label(format!(
                        "heading id `{}` is already used by the heading at line {}",
                        h.id, first_line
                    ))
                    .help("pin a distinct id on one of them with a trailing `{#id}`"),
            );
        } else {
            seen.insert(&h.id, h.span);
        }
    }
}

/// A same-page `@link[..](#frag)` must name a heading id in this document.
/// Fragments of other pages and external URLs are not checked here.
fn check_fragment_links(headings: &[Heading], doc: &Document, diags: &mut Vec<Diagnostic>) {
    // Built on the first fragment link: most documents have none.
    let mut ids: Option<BTreeSet<&str>> = None;
    let mut check = |node: &Inline| {
        let Inline::Shortcode {
            name, args, span, ..
        } = node
        else {
            return;
        };
        if name != "link" {
            return;
        }
        // Before resolve binds it, the URL is still positional.
        let url = args
            .keyword
            .get("url")
            .or_else(|| args.positional.first())
            .and_then(|v| v.as_str());
        let Some(frag) = url.and_then(|u| u.trim().strip_prefix('#')) else {
            return;
        };
        let ids = ids.get_or_insert_with(|| headings.iter().map(|h| h.id.as_str()).collect());
        if fragment_resolves(frag, ids) {
            return;
        }
        let slug = crate::slug::slugify(frag);
        let help = if ids.contains(slug.as_str()) {
            format!("did you mean `#{}`?", slug)
        } else if ids.is_empty() {
            "this document has no headings".to_string()
        } else {
            let listed: Vec<&str> = ids.iter().take(10).copied().collect();
            format!("heading ids in this document: {}", listed.join(", "))
        };
        diags.push(
            Diagnostic::new(Code::MissingFragment, *span)
                .label(format!("no heading has the id `{}`", frag))
                .help(help),
        );
    };
    for b in &doc.blocks {
        walk_block_inlines(b, &mut check);
    }
}

/// Browsers match a fragment against element ids both as written and
/// percent-decoded, and treat an empty fragment and `#top` as the top of the
/// page when no element claims them.
fn fragment_resolves(frag: &str, ids: &BTreeSet<&str>) -> bool {
    frag.is_empty()
        || frag.eq_ignore_ascii_case("top")
        || ids.contains(frag)
        || percent_decode(frag).is_some_and(|d| ids.contains(d.as_str()))
}

fn percent_decode(s: &str) -> Option<String> {
    if !s.contains('%') {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s
                .get(i + 1..i + 3)
                .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Visit every inline node in `block`, including nested emphasis and
/// shortcode bodies (footnote bodies among them).
fn walk_block_inlines(block: &Block, f: &mut impl FnMut(&Inline)) {
    match block {
        Block::Heading { content, .. } | Block::Paragraph { content, .. } => {
            walk_inlines(content, f);
        }
        Block::List { items, .. } => {
            for it in items {
                walk_inlines(&it.content, f);
                for c in &it.children {
                    walk_block_inlines(c, f);
                }
            }
        }
        Block::Blockquote { children, .. } | Block::BlockShortcode { children, .. } => {
            for c in children {
                walk_block_inlines(c, f);
            }
        }
        Block::Table { header, rows, .. } => {
            for cell in header
                .cells
                .iter()
                .chain(rows.iter().flat_map(|r| &r.cells))
            {
                walk_inlines(cell, f);
            }
        }
        Block::DefinitionList { items, .. } => {
            for it in items {
                walk_inlines(&it.term, f);
                walk_inlines(&it.definition, f);
            }
        }
        Block::CodeBlock { .. } | Block::HorizontalRule { .. } => {}
    }
}

fn walk_inlines(nodes: &[Inline], f: &mut impl FnMut(&Inline)) {
    for n in nodes {
        f(n);
        match n {
            Inline::Bold { content, .. }
            | Inline::Italic { content, .. }
            | Inline::Underline { content, .. }
            | Inline::Strike { content, .. } => walk_inlines(content, f),
            Inline::Shortcode {
                content: Some(c), ..
            } => walk_inlines(c, f),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse, resolve (so link URLs are bound to `url`) and validate.
    fn check(input: &str) -> Vec<Diagnostic> {
        let src = SourceMap::new("t.brf", input);
        let tokens = crate::lexer::lex(&src).expect("lex");
        let (mut doc, diags) = crate::parser::parse(tokens, &src);
        assert!(diags.is_empty(), "{:?}", diags);
        let reg = crate::shortcode::Registry::with_builtins();
        let diags = crate::resolve::resolve(&mut doc, &reg);
        assert!(diags.is_empty(), "{:?}", diags);
        validate(&doc, &ValidateOpts::default(), &src)
    }

    fn summary(diags: &[Diagnostic]) -> Vec<(Code, Span, &str, &str)> {
        diags
            .iter()
            .map(|d| {
                (
                    d.code,
                    d.span,
                    d.label.as_deref().unwrap_or(""),
                    d.help.as_deref().unwrap_or(""),
                )
            })
            .collect()
    }

    const PIN: &str = "pin a distinct id on one of them with a trailing `{#id}`";

    #[test]
    fn duplicate_automatic_ids_are_b0506() {
        let diags = check("## Setup\n\nText.\n\n## Setup\n");
        assert_eq!(
            summary(&diags),
            [(
                Code::DuplicateHeadingAnchor,
                Span::new(17, 8),
                "heading id `setup` is already used by the heading at line 1",
                PIN
            )]
        );
    }

    #[test]
    fn explicit_anchor_colliding_with_an_automatic_id_is_b0506() {
        let diags = check("## Setup\n\n## Linux {#setup}\n");
        assert_eq!(
            summary(&diags),
            [(
                Code::DuplicateHeadingAnchor,
                Span::new(10, 17),
                "heading id `setup` is already used by the heading at line 1",
                PIN
            )]
        );
    }

    #[test]
    fn duplicate_explicit_anchors_are_still_b0506() {
        let diags = check("## A {#x}\n## B {#x}\n");
        assert_eq!(
            summary(&diags),
            [(
                Code::DuplicateHeadingAnchor,
                Span::new(10, 9),
                "heading id `x` is already used by the heading at line 1",
                PIN
            )]
        );
    }

    #[test]
    fn pinning_an_id_resolves_the_duplicate() {
        assert!(check("## Setup\n\n## Setup {#setup-linux}\n").is_empty());
    }

    #[test]
    fn headings_without_slug_text_collide_on_the_fallback() {
        let diags = check("## ???\n\n## 🚀\n");
        assert_eq!(diags.len(), 1, "{:?}", diags);
        assert_eq!(diags[0].code, Code::DuplicateHeadingAnchor);
        assert_eq!(
            diags[0].label.as_deref(),
            Some("heading id `section` is already used by the heading at line 1")
        );
    }

    #[test]
    fn duplicate_ids_in_nested_blocks_are_b0506() {
        let diags = check("## Notes\n\n@details(summary: \"s\")\n## Notes\n@end\n");
        assert_eq!(diags.len(), 1, "{:?}", diags);
        assert_eq!(diags[0].code, Code::DuplicateHeadingAnchor);
    }

    #[test]
    fn same_page_fragment_links_to_heading_ids_pass() {
        let src = concat!(
            "# Install Steps\n\n",
            "## Linux {#linux-setup}\n\n",
            "## Café\n\n",
            "See @link[steps](#install-steps), @link[linux](#linux-setup), ",
            "@link[cafe](#café) and @link[encoded](#caf%c3%A9).\n\n",
            "@link[top](#) and @link[also top](#top).\n"
        );
        assert!(check(src).is_empty());
    }

    #[test]
    fn missing_same_page_fragment_is_b0606() {
        let diags = check("## Install Steps\n\nSee @link[here](#install).\n");
        assert_eq!(
            summary(&diags),
            [(
                Code::MissingFragment,
                Span::new(22, 21),
                "no heading has the id `install`",
                "heading ids in this document: install-steps"
            )]
        );
    }

    #[test]
    fn malformed_percent_escapes_do_not_decode() {
        assert_eq!(percent_decode("caf%C3%A9").as_deref(), Some("café"));
        assert_eq!(percent_decode("a%+1"), None);
        assert_eq!(percent_decode("a%2"), None);
        assert_eq!(percent_decode("a%FF"), None);
        assert_eq!(percent_decode("plain"), None);
    }

    #[test]
    fn b0606_suggests_the_slug_of_a_mistyped_fragment() {
        let diags = check("## Install Steps\n\nSee @link[here](#Install-Steps).\n");
        assert_eq!(diags.len(), 1, "{:?}", diags);
        assert_eq!(diags[0].code, Code::MissingFragment);
        assert_eq!(
            diags[0].help.as_deref(),
            Some("did you mean `#install-steps`?")
        );
    }

    #[test]
    fn b0606_without_headings_says_so() {
        let diags = check("See @link[here](#nowhere).\n");
        assert_eq!(
            summary(&diags),
            [(
                Code::MissingFragment,
                Span::new(4, 21),
                "no heading has the id `nowhere`",
                "this document has no headings"
            )]
        );
    }

    #[test]
    fn fragment_links_are_checked_everywhere_inline() {
        let src = concat!(
            "# Doc\n\n",
            "- item @link[a](#gone-1)\n\n",
            "@t\n| h |\n| *@link[b](#gone-2)* |\n\n",
            "Note.@footnote[see @link[c](#gone-3)]\n"
        );
        let missing: Vec<String> = check(src)
            .into_iter()
            .filter(|d| d.code == Code::MissingFragment)
            .filter_map(|d| d.label)
            .collect();
        assert_eq!(
            missing,
            [
                "no heading has the id `gone-1`",
                "no heading has the id `gone-2`",
                "no heading has the id `gone-3`"
            ]
        );
    }

    #[test]
    fn fragments_of_other_pages_are_not_checked() {
        let src = concat!(
            "# Doc\n\n",
            "@link[a](other.html#nope) @link[b](https://example.com/#nope) ",
            "@link[c](./#nope)\n"
        );
        assert!(check(src).is_empty());
    }

    #[test]
    fn unresolved_links_are_checked_too() {
        // brief-web validates before resolving, while the URL is positional.
        let src = SourceMap::new("t.brf", "# Doc\n\n@link[a](#nope)\n");
        let tokens = crate::lexer::lex(&src).expect("lex");
        let (doc, _) = crate::parser::parse(tokens, &src);
        let codes: Vec<Code> = validate(&doc, &ValidateOpts::default(), &src)
            .iter()
            .map(|d| d.code)
            .collect();
        assert_eq!(codes, [Code::MissingFragment]);
    }
}
