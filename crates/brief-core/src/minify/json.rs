//! JSON / JSONL minifiers (v0.2). Comment-free formats: the
//! `MinifyOptions::keep_comments` flag is a no-op here.

use super::{MinifyError, MinifyOutput};
use serde_json::Value;

/// Parse `source` as a single JSON document and re-serialize compactly.
/// Whitespace, newlines, and indentation are dropped; field order is
/// preserved (via `serde_json`'s `preserve_order` feature).
pub fn minify_json(source: &str) -> Result<MinifyOutput, MinifyError> {
    let v: Value = serde_json::from_str(source).map_err(|e| MinifyError::new(e.to_string()))?;
    let s = serde_json::to_string(&v).map_err(|e| MinifyError::new(e.to_string()))?;
    Ok(MinifyOutput::body(s))
}

/// Parse `source` as JSONL — one JSON document per line. Empty and
/// whitespace-only lines are dropped from the output. If any line fails to
/// parse, the whole minification fails (caller emits the original block
/// verbatim).
pub fn minify_jsonl(source: &str) -> Result<MinifyOutput, MinifyError> {
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
    Ok(MinifyOutput::body(out))
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
        assert_eq!(out.body, r#"{"a":1,"b":[1,2,3]}"#);
    }

    #[test]
    fn json_preserves_field_order() {
        let out = minify_json(r#"{"z":1,"a":2,"m":3}"#).unwrap();
        assert_eq!(out.body, r#"{"z":1,"a":2,"m":3}"#);
    }

    #[test]
    fn json_unicode_string() {
        let out = minify_json(r#"{ "lang": "日本語" }"#).unwrap();
        assert_eq!(out.body, r#"{"lang":"日本語"}"#);
    }

    #[test]
    fn json_preserves_unicode_escape() {
        // After a parse-then-serialize round-trip serde_json renders the
        // character directly, but the codepoint must survive intact.
        let out = minify_json(r#"{"x":"é"}"#).unwrap();
        let parsed: Value = serde_json::from_str(&out.body).unwrap();
        assert_eq!(parsed["x"], serde_json::json!("é"));
    }

    #[test]
    fn json_empty_object_and_array() {
        assert_eq!(minify_json("{}").unwrap().body, "{}");
        assert_eq!(minify_json("[]").unwrap().body, "[]");
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
        assert_eq!(out.body, s);
    }

    #[test]
    fn json_large_integer() {
        // i64::MAX fits losslessly. serde_json parses this as an integer
        // without precision loss.
        let out = minify_json("9223372036854775807").unwrap();
        assert_eq!(out.body, "9223372036854775807");
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
        assert_eq!(out.body, "{\"a\":1}\n{\"b\":2}\n{\"c\":3}");
    }

    #[test]
    fn jsonl_drops_blank_lines() {
        let src = "{\"a\":1}\n\n   \n{\"b\":2}\n";
        let out = minify_jsonl(src).unwrap();
        assert_eq!(out.body, "{\"a\":1}\n{\"b\":2}");
    }

    #[test]
    fn jsonl_invalid_line_fails() {
        let src = "{\"a\":1}\nnot json\n{\"b\":2}\n";
        let r = minify_jsonl(src);
        assert!(r.is_err());
        // Error message should reference the offending line number.
        assert!(r.unwrap_err().message.contains("line 2"));
    }
}
