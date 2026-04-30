//! Code-block minifiers used by the LLM emit pass.
//!
//! v0.2 ships JSON and JSONL only. Each minifier is fail-closed: any parse
//! error returns `Err`, and the caller falls back to verbatim emission with
//! a B0701 warning. Minification is required to be semantically lossless —
//! `parse → minify → re-parse` must produce structurally equal data.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinifyError {
    pub message: String,
}

impl MinifyError {
    fn new(s: impl Into<String>) -> Self {
        MinifyError { message: s.into() }
    }
}

/// Returns true if `lang`, lowercased, is one of the minifiers shipped in
/// this build. v0.2: `json`, `jsonl`. Used by the LLM emit pass to decide
/// whether to attempt minification.
pub fn is_supported(lang: &str) -> bool {
    matches!(lang.to_ascii_lowercase().as_str(), "json" | "jsonl")
}

/// Dispatch to the appropriate minifier by language tag. Caller must check
/// `is_supported` first; an unsupported language returns Err.
pub fn minify(lang: &str, source: &str) -> Result<String, MinifyError> {
    match lang.to_ascii_lowercase().as_str() {
        "json" => minify_json(source),
        "jsonl" => minify_jsonl(source),
        other => Err(MinifyError::new(format!(
            "no minifier registered for language `{}`",
            other
        ))),
    }
}

/// Parse `source` as a single JSON document and re-serialize compactly.
/// Whitespace, newlines, and indentation are dropped; field order is
/// preserved (via `serde_json`'s `preserve_order` feature).
pub fn minify_json(source: &str) -> Result<String, MinifyError> {
    let v: Value = serde_json::from_str(source).map_err(|e| MinifyError::new(e.to_string()))?;
    serde_json::to_string(&v).map_err(|e| MinifyError::new(e.to_string()))
}

/// Parse `source` as JSONL — one JSON document per line. Empty and
/// whitespace-only lines are dropped from the output. If any line fails to
/// parse, the whole minification fails (caller emits the original block
/// verbatim).
pub fn minify_jsonl(source: &str) -> Result<String, MinifyError> {
    let mut out = String::with_capacity(source.len());
    let mut first = true;
    for (i, line) in source.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .map_err(|e| MinifyError::new(format!("line {}: {}", i + 1, e)))?;
        let s = serde_json::to_string(&v).map_err(|e| MinifyError::new(e.to_string()))?;
        if !first {
            out.push('\n');
        }
        out.push_str(&s);
        first = false;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_strips_whitespace() {
        let out = minify_json(
            r#"{
            "a": 1,
            "b": [1, 2, 3]
        }"#,
        )
        .unwrap();
        assert_eq!(out, r#"{"a":1,"b":[1,2,3]}"#);
    }

    #[test]
    fn json_preserves_field_order() {
        let out = minify_json(r#"{"z":1,"a":2,"m":3}"#).unwrap();
        assert_eq!(out, r#"{"z":1,"a":2,"m":3}"#);
    }

    #[test]
    fn json_unicode_string() {
        let out = minify_json(r#"{ "lang": "日本語" }"#).unwrap();
        assert_eq!(out, r#"{"lang":"日本語"}"#);
    }

    #[test]
    fn json_preserves_unicode_escape() {
        // After a parse-then-serialize round-trip serde_json renders the
        // character directly, but the codepoint must survive intact.
        let out = minify_json(r#"{"x":"é"}"#).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["x"], serde_json::json!("é"));
    }

    #[test]
    fn json_empty_object_and_array() {
        assert_eq!(minify_json("{}").unwrap(), "{}");
        assert_eq!(minify_json("[]").unwrap(), "[]");
    }

    #[test]
    fn json_deeply_nested() {
        let mut s = String::new();
        for _ in 0..50 {
            s.push('[');
        }
        s.push('1');
        for _ in 0..50 {
            s.push(']');
        }
        let out = minify_json(&s).unwrap();
        assert_eq!(out, s);
    }

    #[test]
    fn json_large_integer() {
        // i64::MAX fits losslessly. serde_json parses this as an integer
        // without precision loss.
        let out = minify_json("9223372036854775807").unwrap();
        assert_eq!(out, "9223372036854775807");
    }

    #[test]
    fn json_invalid_returns_error() {
        let r = minify_json("{ not valid json }");
        assert!(r.is_err());
    }

    #[test]
    fn jsonl_one_per_line() {
        let src = "{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n";
        let out = minify_jsonl(src).unwrap();
        assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n{\"c\":3}");
    }

    #[test]
    fn jsonl_drops_blank_lines() {
        let src = "{\"a\":1}\n\n   \n{\"b\":2}\n";
        let out = minify_jsonl(src).unwrap();
        assert_eq!(out, "{\"a\":1}\n{\"b\":2}");
    }

    #[test]
    fn jsonl_invalid_line_fails() {
        let src = "{\"a\":1}\nnot json\n{\"b\":2}\n";
        let r = minify_jsonl(src);
        assert!(r.is_err());
        // Error message should reference the offending line number.
        assert!(r.unwrap_err().message.contains("line 2"));
    }

    #[test]
    fn dispatch_by_language() {
        assert!(minify("json", "{}").is_ok());
        assert!(minify("JSON", "{}").is_ok());
        assert!(minify("jsonl", "").is_ok());
        assert!(minify("python", "x = 1").is_err());
    }

    #[test]
    fn supported_check() {
        assert!(is_supported("json"));
        assert!(is_supported("JSONL"));
        assert!(!is_supported("rust"));
        assert!(!is_supported(""));
    }
}
