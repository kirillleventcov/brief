//! Parses `SUMMARY.brf` into a navigation tree.
//!
//! Modeled on mdBook's `SUMMARY.md`. The file *is* the navigation: list items
//! become pages, nested lists become sub-pages, headings become section
//! dividers in the sidebar. Each page is described by an inline `@ref`
//! shortcode of the form `@ref[path](title)`.

use brief::ast::{Block, Document, Inline, ListItem, ResolvedRef};
use brief::span::Span;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// One top-level item of the navigation, in document order.
#[derive(Clone, Debug)]
pub enum Entry {
    /// A sidebar section divider, from a `##`-or-deeper heading.
    Section(String),
    Page(PageEntry),
}

/// A page in the navigation, with its nested sub-pages.
#[derive(Clone, Debug)]
pub struct PageEntry {
    /// Source-relative path of the page's `.brf` file.
    pub path: PathBuf,
    pub title: String,
    /// The entry's `@ref` in `SUMMARY.brf`, for diagnostics about the entry
    /// itself. `Span::DUMMY` for the synthesized single-page entry.
    pub span: Span,
    pub children: Vec<PageEntry>,
}

impl PageEntry {
    /// This entry followed by all its descendants, depth-first.
    #[must_use]
    pub fn flatten(&self) -> Vec<&PageEntry> {
        let mut out = Vec::new();
        self.flatten_into(&mut out);
        out
    }

    fn flatten_into<'a>(&'a self, out: &mut Vec<&'a PageEntry>) {
        out.push(self);
        for c in &self.children {
            c.flatten_into(out);
        }
    }
}

/// Walk a parsed `SUMMARY.brf` document, extracting navigation entries in
/// document order.
///
/// `doc` must have been resolved against the site's project index
/// (`Project::check`). Entries are taken from `doc.resolved_refs`, so every
/// page path has already passed the resolver's target rules (relative, `/`
/// separators, no `.` or `..` segments, ends in `.brf`) and names a file
/// that exists under the source root. A `@ref` that did not resolve has its
/// own diagnostic and contributes no entry.
#[must_use]
pub fn extract(doc: &Document) -> Vec<Entry> {
    let resolved = &doc.resolved_refs;
    let mut entries = Vec::new();
    for block in &doc.blocks {
        match block {
            Block::Heading { content, level, .. } if *level >= 2 => {
                let txt = inline_text(content).trim().to_string();
                if !txt.is_empty() {
                    entries.push(Entry::Section(txt));
                }
            }
            Block::List { items, .. } => {
                for it in items {
                    if let Some(p) = parse_list_item(it, resolved) {
                        entries.push(Entry::Page(p));
                    }
                }
            }
            // Top-level `# Summary` heading and stray paragraphs are ignored.
            _ => {}
        }
    }
    entries
}

fn parse_list_item(item: &ListItem, resolved: &BTreeMap<Span, ResolvedRef>) -> Option<PageEntry> {
    let page = find_page_shortcode(&item.content, resolved)?;
    let mut children = Vec::new();
    for child in &item.children {
        if let Block::List { items, .. } = child {
            for sub in items {
                if let Some(p) = parse_list_item(sub, resolved) {
                    children.push(p);
                }
            }
        }
    }
    Some(PageEntry {
        path: page.path,
        title: page.title,
        span: page.span,
        children,
    })
}

struct ParsedPage {
    path: PathBuf,
    title: String,
    span: Span,
}

fn find_page_shortcode(
    inlines: &[Inline],
    resolved: &BTreeMap<Span, ResolvedRef>,
) -> Option<ParsedPage> {
    for n in inlines {
        if let Inline::Shortcode { name, span, .. } = n {
            if name != "ref" {
                continue;
            }
            let Some(r) = resolved.get(span) else {
                continue;
            };
            // Any `#anchor` is dropped: SUMMARY entries are whole pages.
            let title = r.display.trim();
            let title = if title.is_empty() {
                r.target_path.clone()
            } else {
                title.to_string()
            };
            return Some(ParsedPage {
                path: PathBuf::from(&r.target_path),
                title,
                span: *span,
            });
        }
    }
    None
}

/// The plain text of `inlines`, with all formatting dropped and hard breaks
/// turned into spaces.
#[must_use]
pub fn inline_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for n in inlines {
        push_inline_text(n, &mut out);
    }
    out
}

fn push_inline_text(n: &Inline, out: &mut String) {
    match n {
        Inline::Text { value, .. } | Inline::InlineCode { value, .. } => out.push_str(value),
        Inline::Bold { content, .. }
        | Inline::Italic { content, .. }
        | Inline::Underline { content, .. }
        | Inline::Strike { content, .. } => {
            for c in content {
                push_inline_text(c, out);
            }
        }
        Inline::HardBreak { .. } => out.push(' '),
        Inline::Shortcode {
            content: Some(c), ..
        } => {
            for n in c {
                push_inline_text(n, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brief::project::{Checked, Project, ProjectIndex};
    use brief::span::SourceMap;

    /// Check `src` as `SUMMARY.brf` in a project holding `files`.
    fn check(src: &str, files: &[&str]) -> Checked {
        let index = ProjectIndex {
            anchors: files
                .iter()
                .map(|f| (f.to_string(), Default::default()))
                .collect(),
            ..Default::default()
        };
        let s = SourceMap::new("SUMMARY.brf", src);
        Project::standalone().check(&s, Some(&index), Some("SUMMARY.brf"))
    }

    fn pages(entries: &[Entry]) -> Vec<&PageEntry> {
        entries
            .iter()
            .filter_map(|e| {
                if let Entry::Page(p) = e {
                    Some(p)
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn extract_finds_pages_via_ref_shortcode() {
        let checked = check(
            "# Summary\n\n- @ref[intro.brf](Introduction)\n- @ref[advanced.brf](Advanced)\n",
            &["intro.brf", "advanced.brf"],
        );
        assert!(!checked.has_errors(), "{:?}", checked.diagnostics);
        let entries = extract(&checked.doc.unwrap());
        let pages = pages(&entries);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].path, PathBuf::from("intro.brf"));
        assert_eq!(pages[0].title, "Introduction");
        assert_eq!(pages[1].path, PathBuf::from("advanced.brf"));
        assert_eq!(pages[1].title, "Advanced");
    }

    #[test]
    fn unresolved_targets_contribute_no_entry() {
        // Each of these fails the resolver's target rules (or names no
        // file), so it must never become a path the builder reads or writes.
        for target in [
            "../escape.brf",
            "/abs.brf",
            "a\\\\b.brf",
            "notes.md",
            "./intro.brf",
            "missing.brf",
        ] {
            let checked = check(
                &format!(
                    "# Summary\n\n- @ref[{}](X)\n- @ref[intro.brf](Intro)\n",
                    target
                ),
                &["intro.brf"],
            );
            assert!(checked.has_errors(), "{} should not resolve", target);
            let entries = extract(&checked.doc.unwrap());
            let paths: Vec<_> = pages(&entries).iter().map(|p| p.path.clone()).collect();
            assert_eq!(paths, vec![PathBuf::from("intro.brf")], "{}", target);
        }
    }
}
