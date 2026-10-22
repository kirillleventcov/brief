//! Language-tag routing: which tags are minifiable, which are refused, and
//! the dispatcher that picks the right minifier.

use super::{
    MinifyError, MinifyOptions, MinifyOutput, c_cpp, go, java, javascript, json, rust, sql,
};

/// Returns true if `lang`, lowercased, is one of the minifiers shipped in
/// this build. v0.3: json, jsonl, rust/rs, c/h, cpp/c++/cc/cxx/hpp/hxx,
/// java, go, js/javascript, ts/typescript, sql.
#[must_use]
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
#[must_use]
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

/// Dispatch to the appropriate minifier by language tag (case-insensitive).
///
/// Callers should check [`is_supported`] first.
///
/// # Errors
///
/// Returns [`MinifyError`] if `lang` has no registered minifier or the
/// selected minifier fails to tokenize/validate `source`.
pub fn minify(lang: &str, source: &str, opts: &MinifyOptions) -> Result<MinifyOutput, MinifyError> {
    match lang.to_ascii_lowercase().as_str() {
        "json" => json::minify_json(source),
        "jsonl" => json::minify_jsonl(source),
        "rust" | "rs" => rust::minify(source, opts),
        "c" | "h" => c_cpp::minify(source, opts, c_cpp::Dialect::C),
        "cpp" | "c++" | "cc" | "cxx" | "hpp" | "hxx" => {
            c_cpp::minify(source, opts, c_cpp::Dialect::Cpp)
        }
        "java" => java::minify(source, opts),
        "go" => go::minify(source, opts),
        "js" | "javascript" | "ts" | "typescript" => javascript::minify(source, opts),
        "sql" => sql::minify(source, opts),
        other => Err(MinifyError::new(format!(
            "no minifier registered for language `{other}`"
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
