pub mod html;
pub mod llm;

use crate::ast::{Block, Document, Inline};
use crate::shortcode::Registry;

pub fn to_html(doc: &Document, reg: &Registry) -> String {
    html::render(doc, reg)
}

pub fn to_llm(doc: &Document, reg: &Registry, opts: &llm::Opts) -> (String, Vec<String>) {
    llm::render(doc, reg, opts)
}

/// The TeX source of an inline `@math[...]`. The parser keeps math bodies
/// verbatim as plain text, so emitters must output this as-is (escaped once
/// for HTML) rather than run it through inline rendering.
pub(crate) fn math_source(content: &[Inline]) -> String {
    let mut out = String::new();
    for n in content {
        if let Inline::Text { value, .. } = n {
            out.push_str(value);
        }
    }
    out
}

/// The TeX source of a block `@math ... @end` (one text paragraph).
pub(crate) fn math_block_source(children: &[Block]) -> String {
    let mut out = String::new();
    for c in children {
        if let Block::Paragraph { content, .. } = c {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&math_source(content));
        }
    }
    out
}
