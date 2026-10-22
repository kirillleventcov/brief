//! Raw TeX extraction for `@math`, shared by the HTML and LLM emitters.

use crate::ast::{Block, Inline};

/// The TeX source of an inline `@math[...]`. The parser keeps math bodies
/// verbatim as plain text, so emitters must output this as-is (escaped once
/// for HTML) rather than run it through inline rendering.
pub(crate) fn inline_source(content: &[Inline]) -> String {
    let mut out = String::new();
    for n in content {
        if let Inline::Text { value, .. } = n {
            out.push_str(value);
        }
    }
    out
}

/// The TeX source of a block `@math ... @end` (one text paragraph).
pub(crate) fn block_source(children: &[Block]) -> String {
    let mut out = String::new();
    for c in children {
        if let Block::Paragraph { content, .. } = c {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&inline_source(content));
        }
    }
    out
}
