//! The result of a successful minification.

/// A successful minification produces a body plus zero-or-more
/// per-conversion warnings (e.g. B0703 line-comment-converted notices).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct MinifyOutput {
    /// The minified source text.
    pub body: String,
    /// Non-fatal notices produced while minifying, in source order.
    pub warnings: Vec<MinifyWarning>,
}

impl MinifyOutput {
    /// Builds an output carrying `body` and the given `warnings`.
    pub(crate) fn new(body: String, warnings: Vec<MinifyWarning>) -> Self {
        MinifyOutput { body, warnings }
    }

    /// Builds an output carrying `body` and no warnings.
    pub(crate) fn from_body(body: String) -> Self {
        Self::new(body, Vec::new())
    }
}

/// Per-block warnings the minifier may produce. The LLM emit pass is
/// responsible for translating these into stderr `warning[Bxxxx]: …` lines.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum MinifyWarning {
    /// A `//` line comment was rewritten into `/* */` block-comment form so
    /// it survived a single-line minification. Any `*/` inside the comment
    /// body is defused to `* /` so the block cannot close early.
    LineCommentConverted,
}
