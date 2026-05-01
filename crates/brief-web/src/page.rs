//! `@page` shortcode handling.
//!
//! Registered as an inline shortcode at the registry level so the Brief
//! resolver does not flag it as unknown. After compilation, we walk the AST
//! and rewrite each `@page[path#anchor](title)` invocation into a built-in
//! `@link[title](url)` invocation whose URL is resolved against the site map
//! produced by the builder.

use brief::ast::{Block, Document, Inline, ShortArgs};
use brief::shortcode::{ArgSpec, ArgType, ArgValue, Registry, ShortKindOpt, Shortcode};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Adds the `@page[path](title)` inline shortcode to the registry so the
/// resolver accepts it.
pub fn register(reg: &mut Registry) {
    let mut args = std::collections::BTreeMap::new();
    args.insert(
        "title".into(),
        ArgSpec {
            ty: ArgType::String,
            required: false,
            position: Some(1),
            oneof: None,
        },
    );
    reg.map.insert(
        "page".into(),
        Shortcode {
            kind: ShortKindOpt::Inline,
            arguments: args,
            template_html: None,
            template_llm: None,
        },
    );
}

/// Site index used to resolve `@page` targets.
#[derive(Clone, Debug, Default)]
pub struct SiteIndex {
    /// Source-relative `.brf` path → output URL (relative to `base_url`).
    pub pages: BTreeMap<PathBuf, String>,
    /// Source-relative `.brf` path → set of heading anchors that exist in it.
    pub anchors: BTreeMap<PathBuf, BTreeSet<String>>,
    pub base_url: String,
}

#[derive(Debug)]
pub struct PageError {
    pub source_path: PathBuf,
    pub target: String,
    pub kind: PageErrorKind,
}

#[derive(Debug)]
pub enum PageErrorKind {
    UnknownPage,
    UnknownAnchor,
}

impl std::fmt::Display for PageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.kind {
            PageErrorKind::UnknownPage => "unknown page",
            PageErrorKind::UnknownAnchor => "unknown anchor",
        };
        write!(
            f,
            "{}: @page[{}] in {}",
            what,
            self.target,
            self.source_path.display()
        )
    }
}

/// Walks the document and rewrites `@page` shortcodes to `@link` shortcodes
/// resolved against `index`. The current page's source-relative path is used
/// to interpret the `@page[...]` target path.
pub fn rewrite(doc: &mut Document, index: &SiteIndex, source: &Path) -> Vec<PageError> {
    let mut errs = Vec::new();
    for block in &mut doc.blocks {
        rewrite_block(block, index, source, &mut errs);
    }
    errs
}

fn rewrite_block(block: &mut Block, idx: &SiteIndex, source: &Path, errs: &mut Vec<PageError>) {
    match block {
        Block::Heading { content, .. } | Block::Paragraph { content, .. } => {
            for n in content {
                rewrite_inline(n, idx, source, errs);
            }
        }
        Block::List { items, .. } => {
            for it in items {
                for n in &mut it.content {
                    rewrite_inline(n, idx, source, errs);
                }
                for c in &mut it.children {
                    rewrite_block(c, idx, source, errs);
                }
            }
        }
        Block::Blockquote { children, .. } | Block::BlockShortcode { children, .. } => {
            for c in children {
                rewrite_block(c, idx, source, errs);
            }
        }
        Block::Table { header, rows, .. } => {
            for cell in &mut header.cells {
                for n in cell {
                    rewrite_inline(n, idx, source, errs);
                }
            }
            for row in rows {
                for cell in &mut row.cells {
                    for n in cell {
                        rewrite_inline(n, idx, source, errs);
                    }
                }
            }
        }
        Block::CodeBlock { .. } | Block::HorizontalRule { .. } => {}
    }
}

fn rewrite_inline(node: &mut Inline, idx: &SiteIndex, source: &Path, errs: &mut Vec<PageError>) {
    if let Inline::Shortcode {
        name,
        args,
        content,
        ..
    } = node
    {
        if name == "page" {
            let target = content
                .as_ref()
                .map(|c| crate::summary::inline_text(c))
                .unwrap_or_default();
            let title = title_text(args, content);
            let resolution = resolve_target(idx, source, &target);
            match resolution {
                Ok(url) => {
                    *name = "link".to_string();
                    let mut new_args = ShortArgs::default();
                    new_args.keyword.insert("url".into(), ArgValue::Str(url));
                    *args = new_args;
                    *content = Some(vec![Inline::Text {
                        value: title,
                        span: brief::span::Span::new(0, 0),
                    }]);
                }
                Err(kind) => {
                    errs.push(PageError {
                        source_path: source.to_path_buf(),
                        target: target.clone(),
                        kind,
                    });
                    // Render as plain text so the build can still produce an
                    // (imperfect) HTML page for development.
                    *node = Inline::Text {
                        value: title,
                        span: brief::span::Span::new(0, 0),
                    };
                }
            }
            return;
        }
        if let Some(c) = content {
            for inner in c {
                rewrite_inline(inner, idx, source, errs);
            }
        }
    } else if let Inline::Bold { content, .. }
    | Inline::Italic { content, .. }
    | Inline::Underline { content, .. }
    | Inline::Strike { content, .. } = node
    {
        for inner in content {
            rewrite_inline(inner, idx, source, errs);
        }
    }
}

fn title_text(args: &ShortArgs, content: &Option<Vec<Inline>>) -> String {
    if let Some(v) = args.keyword.get("title").and_then(arg_str) {
        return v.to_string();
    }
    if let Some(v) = args.positional.first().and_then(arg_str) {
        return v.to_string();
    }
    content
        .as_ref()
        .map(|c| crate::summary::inline_text(c))
        .unwrap_or_default()
}

fn arg_str(v: &ArgValue) -> Option<&str> {
    match v {
        ArgValue::Str(s) | ArgValue::Ident(s) => Some(s.as_str()),
        _ => None,
    }
}

fn resolve_target(idx: &SiteIndex, source: &Path, target: &str) -> Result<String, PageErrorKind> {
    let (path_part, anchor) = match target.split_once('#') {
        Some((p, a)) => (p, Some(a)),
        None => (target, None),
    };
    let path_part = path_part.trim();
    let candidate = if path_part.is_empty() {
        source.to_path_buf()
    } else {
        // Resolve relative to the current page's directory.
        let parent = source.parent().unwrap_or_else(|| Path::new(""));
        normalize(&parent.join(path_part))
    };
    let url = idx
        .pages
        .get(&candidate)
        .ok_or(PageErrorKind::UnknownPage)?;
    let mut full = if url.starts_with('/') || url.contains("://") {
        url.clone()
    } else {
        let mut s = idx.base_url.clone();
        if !s.ends_with('/') {
            s.push('/');
        }
        s.push_str(url);
        s
    };
    if let Some(a) = anchor {
        if !a.is_empty() {
            let anchors = idx
                .anchors
                .get(&candidate)
                .ok_or(PageErrorKind::UnknownAnchor)?;
            if !anchors.contains(a) {
                return Err(PageErrorKind::UnknownAnchor);
            }
            full.push('#');
            full.push_str(a);
        }
    }
    Ok(full)
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Walks a document and collects the heading anchors it exposes.
pub fn collect_anchors(doc: &Document) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for b in &doc.blocks {
        collect_anchors_block(b, &mut set);
    }
    set
}

fn collect_anchors_block(b: &Block, set: &mut BTreeSet<String>) {
    match b {
        Block::Heading { anchor, .. } => {
            if let Some(a) = anchor {
                set.insert(a.clone());
            }
        }
        Block::Blockquote { children, .. } | Block::BlockShortcode { children, .. } => {
            for c in children {
                collect_anchors_block(c, set);
            }
        }
        Block::List { items, .. } => {
            for it in items {
                for c in &it.children {
                    collect_anchors_block(c, set);
                }
            }
        }
        _ => {}
    }
}
