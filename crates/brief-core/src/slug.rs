//! Heading ids.
//!
//! Every heading has an id: its explicit `{#id}` when it declares one, else
//! the GitHub-compatible slug of its text. The HTML emitter, the project
//! index behind `@ref`, same-page `#fragment` link checks and the
//! duplicate-id check all derive ids here, so an id a link is allowed to
//! target is exactly an id the emitter writes.
//!
//! Duplicate ids are an error (B0506) rather than GitHub's `-1` suffixes, so
//! a heading's id depends only on the heading itself and never shifts when
//! another heading is added above it.

use crate::ast::{Block, Document, Inline};
use crate::span::Span;

/// Id of a heading whose text slugs to nothing (only punctuation, symbols
/// or emoji). A second such heading collides with the first and must pin
/// its own id with `{#id}`.
pub const EMPTY_SLUG_ID: &str = "section";

/// One heading in document order, with the id it is emitted under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heading {
    pub level: u8,
    /// Plain text of the heading (see `heading_text`).
    pub text: String,
    /// Effective id: the explicit `{#id}`, else `slugify(text)`, else
    /// `EMPTY_SLUG_ID`.
    pub id: String,
    /// `true` when `id` comes from an explicit `{#id}`.
    pub explicit: bool,
    pub span: Span,
}

/// GitHub's heading slug: lowercase, drop every character that is not a
/// letter, digit, space, `-` or `_`, then turn each space into `-`. Runs of
/// spaces are not collapsed, so `a  b` is `a--b`, as on GitHub.
pub fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // Most headings are ASCII: skip `to_lowercase`'s intermediate copy.
    // Same result, since ASCII alphanumerics are exactly [A-Za-z0-9].
    if text.is_ascii() {
        for b in text.bytes() {
            match b {
                b' ' => out.push('-'),
                b'A'..=b'Z' => out.push(b.to_ascii_lowercase() as char),
                b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => out.push(b as char),
                _ => {}
            }
        }
        return out;
    }
    for c in text.to_lowercase().chars() {
        if c == ' ' {
            out.push('-');
        } else if c.is_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        }
    }
    out
}

/// `true` if `s` is an id some heading can carry: non-empty and unchanged by
/// `slugify`. Covers every explicit `{#id}` and every automatic id.
pub fn is_heading_id(s: &str) -> bool {
    !s.is_empty() && slugify(s) == s
}

/// The visible text of heading content with inline markup flattened. Code
/// spans contribute their text; `@ref` contributes its title (its body is a
/// path, not prose); footnote bodies render at the page foot and contribute
/// nothing. Leading and trailing whitespace is trimmed.
pub fn heading_text(content: &[Inline]) -> String {
    let mut out = String::new();
    push_text(content, &mut out);
    out.trim().to_string()
}

fn push_text(nodes: &[Inline], out: &mut String) {
    for n in nodes {
        match n {
            Inline::Text { value, .. } | Inline::InlineCode { value, .. } => out.push_str(value),
            Inline::Bold { content, .. }
            | Inline::Italic { content, .. }
            | Inline::Underline { content, .. }
            | Inline::Strike { content, .. } => push_text(content, out),
            Inline::HardBreak { .. } => out.push(' '),
            Inline::Shortcode {
                name,
                args,
                content,
                ..
            } => match name.as_str() {
                "ref" => {
                    // Before resolve binds it, the title is still positional.
                    let title = args
                        .keyword
                        .get("title")
                        .or_else(|| args.positional.first())
                        .and_then(|v| v.as_str());
                    if let Some(t) = title {
                        out.push_str(t);
                    }
                }
                "footnote" => {}
                _ => {
                    if let Some(c) = content {
                        push_text(c, out);
                    }
                }
            },
        }
    }
}

/// The id a heading is emitted under: `anchor` if it has one, else the slug
/// of its text, else `EMPTY_SLUG_ID`.
pub fn heading_id(content: &[Inline], anchor: Option<&str>) -> String {
    match anchor {
        Some(a) => a.to_string(),
        None => auto_id(&heading_text(content)),
    }
}

fn auto_id(text: &str) -> String {
    let slug = slugify(text);
    if slug.is_empty() {
        EMPTY_SLUG_ID.to_string()
    } else {
        slug
    }
}

/// Every heading in `doc`, in document order, including headings nested in
/// blockquotes, list items and block shortcodes.
pub fn headings(doc: &Document) -> Vec<Heading> {
    let mut out = Vec::new();
    for b in &doc.blocks {
        collect(b, &mut out);
    }
    out
}

fn collect(block: &Block, out: &mut Vec<Heading>) {
    match block {
        Block::Heading {
            level,
            content,
            anchor,
            span,
        } => {
            let text = heading_text(content);
            let id = match anchor {
                Some(a) => a.clone(),
                None => auto_id(&text),
            };
            out.push(Heading {
                level: *level,
                text,
                id,
                explicit: anchor.is_some(),
                span: *span,
            });
        }
        Block::Blockquote { children, .. } | Block::BlockShortcode { children, .. } => {
            for c in children {
                collect(c, out);
            }
        }
        Block::List { items, .. } => {
            for it in items {
                for c in &it.children {
                    collect(c, out);
                }
            }
        }
        Block::Paragraph { .. }
        | Block::CodeBlock { .. }
        | Block::Table { .. }
        | Block::DefinitionList { .. }
        | Block::HorizontalRule { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Document {
        let src = crate::span::SourceMap::new("t.brf", src);
        let tokens = crate::lexer::lex(&src).expect("lex");
        let (doc, diags) = crate::parser::parse(tokens, &src);
        assert!(diags.is_empty(), "{:?}", diags);
        doc
    }

    fn ids(src: &str) -> Vec<String> {
        headings(&parse(src)).into_iter().map(|h| h.id).collect()
    }

    #[test]
    fn slugify_matches_github() {
        assert_eq!(slugify("Install Steps"), "install-steps");
        assert_eq!(slugify("What's new in v0.4?"), "whats-new-in-v04");
        assert_eq!(slugify("Foo  Bar"), "foo--bar");
        assert_eq!(slugify("a - b"), "a---b");
        assert_eq!(slugify("snake_case & kebab-case"), "snake_case--kebab-case");
        assert_eq!(slugify("C++ / Rust (2024)"), "c--rust-2024");
        assert_eq!(slugify("@ref[x.brf]"), "refxbrf");
    }

    #[test]
    fn slugify_is_unicode_aware() {
        assert_eq!(slugify("Café Société"), "café-société");
        assert_eq!(slugify("Привет, Мир"), "привет-мир");
        assert_eq!(slugify("日本語のテキスト"), "日本語のテキスト");
        assert_eq!(slugify("ΣΊΣΥΦΟΣ"), "σίσυφος");
        assert_eq!(slugify("Ünïcödé 🚀 Launch"), "ünïcödé--launch");
        assert_eq!(slugify("no\u{a0}break"), "nobreak");
    }

    #[test]
    fn heading_text_flattens_markup() {
        assert_eq!(
            ids("# *Bold* and _italic_ `code_span()`\n"),
            ["bold-and-italic-code_span"]
        );
        assert_eq!(
            ids("# Press @kbd[Ctrl] and @link[docs](https://x.y)\n"),
            ["press-ctrl-and-docs"]
        );
        assert_eq!(ids("# Notes @footnote[not in the id]\n"), ["notes"]);
        assert_eq!(ids("# See @ref[a.brf](The Guide)\n"), ["see-the-guide"]);
    }

    #[test]
    fn explicit_anchor_overrides_the_slug() {
        let hs = headings(&parse("# Install Steps {#setup}\n\n## Other\n"));
        assert_eq!(
            hs,
            vec![
                Heading {
                    level: 1,
                    text: "Install Steps".into(),
                    id: "setup".into(),
                    explicit: true,
                    span: hs[0].span,
                },
                Heading {
                    level: 2,
                    text: "Other".into(),
                    id: "other".into(),
                    explicit: false,
                    span: hs[1].span,
                },
            ]
        );
    }

    #[test]
    fn empty_slug_falls_back_to_section() {
        assert_eq!(ids("# ???\n\n## 🚀\n"), ["section", "section"]);
        assert_eq!(heading_id(&[], None), "section");
    }

    #[test]
    fn nested_headings_are_collected_in_order() {
        assert_eq!(
            ids(concat!(
                "# One\n\n",
                "@callout(kind: \"note\")\n## In Callout\n\n",
                "@details(summary: \"s\")\n### Inside\n@end\n@end\n"
            )),
            ["one", "in-callout", "inside"]
        );
    }

    #[test]
    fn is_heading_id_accepts_every_slug_and_nothing_else() {
        for s in [
            "install-steps",
            "café",
            "snake_case",
            "v04",
            "section",
            "a--b",
        ] {
            assert!(is_heading_id(s), "{}", s);
        }
        for s in ["", "Install", "a b", "a.b", "a#b", "a/b"] {
            assert!(!is_heading_id(s), "{}", s);
        }
    }
}
