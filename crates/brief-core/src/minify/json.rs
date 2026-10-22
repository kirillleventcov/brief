//! JSON / JSONL minifiers (v0.2). Comment-free formats: the
//! `MinifyOptions::keep_comments` flag is a no-op here.
//!
//! serde_json only validates; the output is the source with insignificant
//! whitespace removed lexically. Re-serializing a parsed `Value` would round
//! numbers through `f64` (`1.50` → `1.5`, big integers → exponent form,
//! `1e-400` → `0.0`), rewrite string escapes, and keep only the last of
//! duplicate keys — the model would see different data than the author
//! wrote.

use super::{MinifyError, MinifyOutput};
use serde::de::IgnoredAny;

/// Validate `source` as a single JSON document and strip insignificant
/// whitespace. Number text, string escapes, key order and duplicate keys are
/// kept byte-for-byte.
///
/// # Errors
///
/// Returns [`MinifyError`] carrying serde_json's message if `source` is not
/// valid JSON.
pub fn minify_json(source: &str) -> Result<MinifyOutput, MinifyError> {
    serde_json::from_str::<IgnoredAny>(source).map_err(|e| MinifyError::new(e.to_string()))?;
    let mut out = String::with_capacity(source.len());
    strip_whitespace(source, &mut out);
    Ok(MinifyOutput::from_body(out))
}

/// Validate `source` as JSONL — one JSON document per line — and strip each
/// line's insignificant whitespace, keeping the line structure. Lines made
/// only of JSON whitespace are dropped from the output. If any line fails to
/// parse, the whole minification fails (caller emits the original block
/// verbatim).
///
/// # Errors
///
/// Returns [`MinifyError`] naming the 1-based line number of the first line
/// that is not valid JSON.
pub fn minify_jsonl(source: &str) -> Result<MinifyOutput, MinifyError> {
    let mut out = String::with_capacity(source.len());
    let mut first = true;
    for (i, line) in source.lines().enumerate() {
        // JSON's own whitespace set, not `str::trim`'s Unicode one: a line
        // holding only e.g. U+00A0 is not JSON and must fail validation
        // rather than vanish.
        if line.bytes().all(is_json_whitespace) {
            continue;
        }
        serde_json::from_str::<IgnoredAny>(line)
            .map_err(|e| MinifyError::new(format!("line {}: {e}", i + 1)))?;
        if !first {
            out.push('\n');
        }
        strip_whitespace(line, &mut out);
        first = false;
    }
    Ok(MinifyOutput::from_body(out))
}

fn is_json_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Append `json` to `out` minus the whitespace between tokens. `json` must
/// already be valid JSON: string boundaries are found by tracking `"` and
/// `\` escapes, and string contents are copied untouched. Only ASCII bytes
/// are dropped, so every slice boundary is a char boundary.
fn strip_whitespace(json: &str, out: &mut String) {
    let bytes = json.as_bytes();
    let mut copy_from = 0;
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            match b {
                b'\\' => i += 1,
                b'"' => in_string = false,
                _ => {}
            }
        } else if b == b'"' {
            in_string = true;
        } else if is_json_whitespace(b) {
            out.push_str(&json[copy_from..i]);
            copy_from = i + 1;
        }
        i += 1;
    }
    out.push_str(&json[copy_from..]);
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
        let out = minify_json(r#"{ "x": "\u00e9\ud83d\ude00" }"#).unwrap();
        assert_eq!(out.body, r#"{"x":"\u00e9\ud83d\ude00"}"#);
    }

    #[test]
    fn json_empty_object_and_array() {
        assert_eq!(minify_json("{}").unwrap().body, "{}");
        assert_eq!(minify_json("[]").unwrap().body, "[]");
        assert_eq!(minify_json("{ }").unwrap().body, "{}");
        assert_eq!(minify_json("[\n]").unwrap().body, "[]");
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
        let out = minify_json("9223372036854775807").unwrap();
        assert_eq!(out.body, "9223372036854775807");
    }

    #[test]
    fn json_number_text_kept_exactly() {
        // Beyond u64, trailing zeros, underflow, overflow, negative zero and
        // exponent spelling all survive as written.
        let out = minify_json(
            r#"[ 12345678901234567890123, 1.50, 1e-400, 1E+400, -0, -0.0, 2.5e3, 0.1 ]"#,
        )
        .unwrap();
        assert_eq!(
            out.body,
            "[12345678901234567890123,1.50,1e-400,1E+400,-0,-0.0,2.5e3,0.1]"
        );
    }

    #[test]
    fn json_duplicate_keys_kept() {
        let out =
            minify_json(r#"{"id": 12345678901234567890123, "f": 1.50, "a": 1, "a": 3}"#).unwrap();
        assert_eq!(
            out.body,
            r#"{"id":12345678901234567890123,"f":1.50,"a":1,"a":3}"#
        );
    }

    #[test]
    fn json_escapes_kept_exactly() {
        let out =
            minify_json(r#"{ "q": "say \"hi\"", "b": "C:\\dir\\", "s": "a\/b\n\t" }"#).unwrap();
        assert_eq!(
            out.body,
            r#"{"q":"say \"hi\"","b":"C:\\dir\\","s":"a\/b\n\t"}"#
        );
    }

    #[test]
    fn json_string_whitespace_and_punctuation_kept() {
        // Whitespace and JSON punctuation inside strings are content; an
        // escaped backslash right before the closing quote must not keep the
        // scanner inside the string.
        let out =
            minify_json("{ \"k  ey\" : \"  a , b : [ } { \\\\\" , \"t\" : \"\\\" ]\" }").unwrap();
        assert_eq!(
            out.body,
            "{\"k  ey\":\"  a , b : [ } { \\\\\",\"t\":\"\\\" ]\"}"
        );
    }

    #[test]
    fn json_all_whitespace_kinds_stripped() {
        let out = minify_json("\r\n\t{ \"a\" :\r\n\t[ true , false , null ] }\n").unwrap();
        assert_eq!(out.body, r#"{"a":[true,false,null]}"#);
    }

    #[test]
    fn json_invalid_returns_error() {
        let r = minify_json("{ not valid json }");
        assert!(r.is_err());
        assert!(minify_json("{\"a\":1} trailing").is_err());
        assert!(minify_json("[1,]").is_err());
        // Non-JSON whitespace between tokens is not ours to drop.
        assert!(minify_json("[1,\u{a0}2]").is_err());
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
    fn jsonl_lines_kept_exactly() {
        let src = "{ \"id\": 12345678901234567890123, \"a\": 1, \"a\": 2 }\r\n[ 1.50 , \"x y\" ]\n";
        let out = minify_jsonl(src).unwrap();
        assert_eq!(
            out.body,
            "{\"id\":12345678901234567890123,\"a\":1,\"a\":2}\n[1.50,\"x y\"]"
        );
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
    fn jsonl_non_json_whitespace_line_fails() {
        let r = minify_jsonl("{\"a\":1}\n\u{a0}\n");
        assert!(r.unwrap_err().message.contains("line 2"));
    }
}
