pub mod html;
pub mod llm;

use crate::ast::Document;
use crate::shortcode::Registry;

pub fn to_html(doc: &Document, reg: &Registry) -> String {
    html::render(doc, reg)
}

pub fn to_llm(doc: &Document, reg: &Registry, opts: &llm::Opts) -> String {
    llm::render(doc, reg, opts)
}
