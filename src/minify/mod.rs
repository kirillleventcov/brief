//! Code-block minifiers used by the LLM emit pass.
//!
//! v0.2 shipped JSON/JSONL. v0.3 adds Rust, Go, JavaScript/TypeScript, Java,
//! C/C++, and SQL via hand-rolled tokenizers. Each minifier is fail-closed:
//! any parse error returns `Err`, and the caller falls back to verbatim
//! emission with a B0701 warning.
//!
//! Minification is required to be semantically lossless — the broad rule is
//! `parse → minify → re-parse` produces structurally equal data. Where we
//! lack a real parser we settle for tokenizer-equivalence and a hand-curated
//! corpus of edge-case fixtures.

pub mod c_common;
pub mod c_cpp;
pub mod go;
pub mod java;
pub mod javascript;
pub mod json;
pub mod rust;
pub mod sql;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinifyError {
    pub message: String,
}

impl MinifyError {
    pub(crate) fn new(s: impl Into<String>) -> Self {
        MinifyError { message: s.into() }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MinifyOptions {
    /// State 3 — `@minify-keep-comments`. When true the minifier strips
    /// whitespace as usual but preserves comments, converting `//` line
    /// comments to `/* */` block form (and recording a B0703 warning per
    /// conversion). When false (default), comments are dropped entirely.
    pub keep_comments: bool,
}

/// A successful minification produces a body plus zero-or-more
/// per-conversion warnings (e.g. B0703 line-comment-converted notices).
#[derive(Debug, Clone, Default)]
pub struct MinifyOutput {
    pub body: String,
    pub warnings: Vec<MinifyWarning>,
}

impl MinifyOutput {
    pub(crate) fn body(s: impl Into<String>) -> Self {
        MinifyOutput {
            body: s.into(),
            warnings: Vec::new(),
        }
    }
}

/// Per-block warnings the minifier may produce. The LLM emit pass is
/// responsible for translating these into stderr `warning[Bxxxx]: …` lines.
#[derive(Debug, Clone)]
pub enum MinifyWarning {
    /// A `//` line comment was rewritten into `/* */` block-comment form so
    /// it survived a single-line minification. The user owns the risk that
    /// the comment body contains `*/`.
    LineCommentConverted,
}

/// Returns true if `lang`, lowercased, is one of the minifiers shipped in
/// this build. v0.3: json, jsonl, rust/rs, c/h, cpp/c++/cc/cxx/hpp/hxx,
/// java, go, js/javascript, ts/typescript, sql.
pub fn is_supported(lang: &str) -> bool {
    matches!(
        lang.to_ascii_lowercase().as_str(),
        "json"
            | "jsonl"
            | "rust"
            | "rs"
            | "c"
            | "h"
            | "cpp"
            | "c++"
            | "cc"
            | "cxx"
            | "hpp"
            | "hxx"
            | "java"
            | "go"
            | "js"
            | "javascript"
            | "ts"
            | "typescript"
            | "sql"
    )
}

/// If `lang` is a permanently-refused language (significant whitespace),
/// return a one-line reason. Languages: python/py, yaml/yml, makefile/make/mk.
pub fn refusal_reason(lang: &str) -> Option<&'static str> {
    match lang.to_ascii_lowercase().as_str() {
        "python" | "py" => Some("Python uses significant whitespace; cannot be safely minified"),
        "yaml" | "yml" => Some("YAML uses significant whitespace; cannot be safely minified"),
        "makefile" | "make" | "mk" => Some(
            "Makefile syntax is whitespace-sensitive (tabs are significant); cannot be safely minified",
        ),
        _ => None,
    }
}

/// Dispatch to the appropriate minifier by language tag. Caller must check
/// `is_supported` first; an unsupported language returns Err.
pub fn minify(lang: &str, source: &str, opts: &MinifyOptions) -> Result<MinifyOutput, MinifyError> {
    match lang.to_ascii_lowercase().as_str() {
        "json" => json::minify_json(source),
        "jsonl" => json::minify_jsonl(source),
        "rust" | "rs" => rust::minify(source, opts),
        "c" | "h" => c_cpp::minify(source, opts, false),
        "cpp" | "c++" | "cc" | "cxx" | "hpp" | "hxx" => c_cpp::minify(source, opts, true),
        "java" => java::minify(source, opts),
        "go" => go::minify(source, opts),
        "js" | "javascript" | "ts" | "typescript" => javascript::minify(source, opts),
        "sql" => sql::minify(source, opts),
        other => Err(MinifyError::new(format!(
            "no minifier registered for language `{}`",
            other
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_json_and_aliases() {
        let opts = MinifyOptions::default();
        assert!(minify("json", "{}", &opts).is_ok());
        assert!(minify("JSON", "{}", &opts).is_ok());
        assert!(minify("jsonl", "", &opts).is_ok());
        assert!(minify("python", "x = 1", &opts).is_err());
    }

    #[test]
    fn supported_check() {
        assert!(is_supported("json"));
        assert!(is_supported("JSONL"));
        assert!(is_supported("rust"));
        assert!(is_supported("rs"));
        assert!(is_supported("c++"));
        assert!(is_supported("ts"));
        assert!(!is_supported("python"));
        assert!(!is_supported(""));
    }

    #[test]
    fn refused_languages() {
        assert!(refusal_reason("python").is_some());
        assert!(refusal_reason("PY").is_some());
        assert!(refusal_reason("yaml").is_some());
        assert!(refusal_reason("yml").is_some());
        assert!(refusal_reason("makefile").is_some());
        assert!(refusal_reason("rust").is_none());
        assert!(refusal_reason("json").is_none());
    }
}
