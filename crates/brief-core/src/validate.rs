use crate::ast::{Block, Document};
use crate::diag::{Code, Diagnostic};
use crate::span::{SourceMap, Span};
use std::collections::BTreeMap;

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
    check_duplicate_anchors(doc, src, &mut diags);
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

fn check_duplicate_anchors(doc: &Document, src: &SourceMap, diags: &mut Vec<Diagnostic>) {
    let mut seen: BTreeMap<String, Span> = BTreeMap::new();
    for b in &doc.blocks {
        collect_anchor(b, src, &mut seen, diags);
    }
}

fn collect_anchor(
    block: &Block,
    src: &SourceMap,
    seen: &mut BTreeMap<String, Span>,
    diags: &mut Vec<Diagnostic>,
) {
    match block {
        Block::Heading { anchor, span, .. } => {
            if let Some(name) = anchor {
                if let Some(first_span) = seen.get(name) {
                    let (first_line, _) = src.line_col(first_span.start);
                    diags.push(Diagnostic::new(Code::DuplicateHeadingAnchor, *span).label(
                        format!("anchor `{}` already used at line {}", name, first_line),
                    ));
                } else {
                    seen.insert(name.clone(), *span);
                }
            }
        }
        Block::Blockquote { children, .. } | Block::BlockShortcode { children, .. } => {
            for c in children {
                collect_anchor(c, src, seen, diags);
            }
        }
        Block::List { items, .. } => {
            for it in items {
                for c in &it.children {
                    collect_anchor(c, src, seen, diags);
                }
            }
        }
        _ => {}
    }
}
