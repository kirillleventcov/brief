//! Per-block knobs passed to every minifier.

/// Options controlling a single minification.
///
/// The default drops all comments.
#[derive(Debug, Clone, Default)]
pub struct MinifyOptions {
    /// State 3 — `@minify-keep-comments`. When true the minifier strips
    /// whitespace as usual but preserves comments, converting `//` line
    /// comments to `/* */` block form (and recording a B0703 warning per
    /// conversion). When false (default), comments are dropped entirely.
    pub keep_comments: bool,
}
