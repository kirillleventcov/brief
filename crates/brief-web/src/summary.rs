//! Parses `SUMMARY.brf` into a navigation tree.
//!
//! Modeled on mdBook's `SUMMARY.md`. The file *is* the navigation: list items
//! become pages, nested lists become sub-pages, headings become section
//! dividers in the sidebar. Each page is described by an inline `@page`
//! shortcode of the form `@page[path](title)`.

use brief::ast::{Block, Document, Inline, ListItem};
use brief::shortcode::ArgValue;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum Entry {
    Section(String),
    Page(PageEntry),
}

#[derive(Clone, Debug)]
pub struct PageEntry {
    pub path: PathBuf,
    pub title: String,
    pub children: Vec<PageEntry>,
}

impl PageEntry {
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
pub fn extract(doc: &Document) -> Result<Vec<Entry>, String> {
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
                    if let Some(p) = parse_list_item(it)? {
                        entries.push(Entry::Page(p));
                    }
                }
            }
            // Top-level `# Summary` heading and stray paragraphs are ignored.
            _ => {}
        }
    }
    Ok(entries)
}

fn parse_list_item(item: &ListItem) -> Result<Option<PageEntry>, String> {
    let Some(page) = find_page_shortcode(&item.content) else {
        return Ok(None);
    };
    let mut children = Vec::new();
    for child in &item.children {
        if let Block::List { items, .. } = child {
            for sub in items {
                if let Some(p) = parse_list_item(sub)? {
                    children.push(p);
                }
            }
        }
    }
    Ok(Some(PageEntry {
        path: page.path,
        title: page.title,
        children,
    }))
}

struct ParsedPage {
    path: PathBuf,
    title: String,
}

fn find_page_shortcode(inlines: &[Inline]) -> Option<ParsedPage> {
    for n in inlines {
        if let Inline::Shortcode {
            name,
            args,
            content,
            ..
        } = n
        {
            if name != "page" {
                continue;
            }
            let path_str = content.as_ref().map(|c| inline_text(c)).unwrap_or_default();
            let title_arg = args
                .keyword
                .get("title")
                .and_then(arg_str)
                .map(str::to_string);
            let title_pos = args
                .positional
                .first()
                .and_then(arg_str)
                .map(str::to_string);
            let title = title_arg.or(title_pos).unwrap_or_else(|| path_str.clone());
            // Strip any anchor — SUMMARY entries point to whole pages, not
            // anchors. Anchors are valid in body content.
            let path = path_str.split('#').next().unwrap_or("").trim().to_string();
            if path.is_empty() {
                continue;
            }
            return Some(ParsedPage {
                path: PathBuf::from(path),
                title: title.trim().to_string(),
            });
        }
    }
    None
}

fn arg_str(v: &ArgValue) -> Option<&str> {
    match v {
        ArgValue::Str(s) | ArgValue::Ident(s) => Some(s.as_str()),
        _ => None,
    }
}

pub fn inline_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for n in inlines {
        push_inline_text(n, &mut out);
    }
    out
}

fn push_inline_text(n: &Inline, out: &mut String) {
    match n {
        Inline::Text { value, .. } => out.push_str(value),
        Inline::InlineCode { value, .. } => out.push_str(value),
        Inline::Bold { content, .. }
        | Inline::Italic { content, .. }
        | Inline::Underline { content, .. }
        | Inline::Strike { content, .. } => {
            for c in content {
                push_inline_text(c, out);
            }
        }
        Inline::HardBreak { .. } => out.push(' '),
        Inline::Shortcode { content, .. } => {
            if let Some(c) = content {
                for n in c {
                    push_inline_text(n, out);
                }
            }
        }
    }
}
