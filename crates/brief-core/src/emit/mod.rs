//! Output backends: HTML for browsers and a compact text form for LLMs.
//!
//! Both emitters take a parsed (and ideally resolved) [`Document`] plus the
//! shortcode [`Registry`] whose templates override the built-in rendering.

pub mod html;
pub mod llm;
mod math;

use crate::ast::Document;
use crate::shortcode::Registry;

/// Renders `doc` as an HTML fragment. Same as [`html::render`].
#[must_use]
pub fn to_html(doc: &Document, registry: &Registry) -> String {
    html::render(doc, registry)
}

/// Renders `doc` in the LLM-oriented text form. Same as [`llm::render`].
pub fn to_llm(doc: &Document, registry: &Registry, options: &llm::Options) -> llm::Output {
    llm::render(doc, registry, options)
}
