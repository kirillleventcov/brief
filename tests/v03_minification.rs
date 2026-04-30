//! End-to-end integration tests for v0.3 code-block minification.
//!
//! Each test feeds a Brief document through the full lex → parse →
//! resolve → validate → emit pipeline and asserts on the LLM-mode
//! output and warnings, exercising the minifier dispatcher and code-
//! attribute handling together.

use brief::config::Config;
use brief::emit::llm;
use brief::lexer::lex;
use brief::parser::parse;
use brief::resolve::resolve;
use brief::shortcode::Registry;
use brief::span::SourceMap;
use brief::validate::{ValidateOpts, validate};

fn render_llm(input: &str) -> (String, Vec<String>) {
    let src = SourceMap::new("doc.brf", input);
    let toks = lex(&src).expect("lex ok");
    let (mut doc, mut diags) = parse(toks, &src);
    let cfg = Config::default();
    let registry = Registry::with_builtins();
    diags.extend(resolve(&mut doc, &registry));
    diags.extend(validate(&doc, &ValidateOpts::default()));
    assert!(diags.is_empty(), "compile diags: {:?}", diags);
    let opts = llm::Opts {
        minify_code_blocks: cfg.compile.llm.minify_code_blocks,
        minify_languages: cfg.compile.llm.minify_languages.clone(),
        preserve_code_fences: cfg.compile.llm.preserve_code_fences,
        ..llm::Opts::default()
    };
    llm::render(&doc, &registry, &opts)
}

#[test]
fn rust_block_minified() {
    let input =
        "# Doc\n\n```rust\nfn add(a: i32, b: i32) -> i32 {\n    // sum\n    a + b\n}\n```\n";
    let (out, w) = render_llm(input);
    assert!(w.is_empty(), "{:?}", w);
    assert!(
        out.contains("fn add(a:i32,b:i32)->i32{a+b}"),
        "got: {}",
        out
    );
}

#[test]
fn javascript_preserves_newlines() {
    let input = "```js\nfunction f() {\n    return 1;\n}\n```\n";
    let (out, w) = render_llm(input);
    assert!(w.is_empty(), "{:?}", w);
    assert!(out.contains("function f(){\nreturn 1;\n}"), "got: {}", out);
}

#[test]
fn cpp_alias_with_raw_string() {
    let input = "```cpp\nconst char* s = R\"x(hi)x\";\n```\n";
    let (out, w) = render_llm(input);
    assert!(w.is_empty(), "{:?}", w);
    assert!(out.contains("R\"x(hi)x\""), "got: {}", out);
}

#[test]
fn c_preprocessor_kept_on_own_line() {
    let input = "```c\n#include <stdio.h>\nint main() { return 0; }\n```\n";
    let (out, w) = render_llm(input);
    assert!(w.is_empty(), "{:?}", w);
    assert!(
        out.contains("#include <stdio.h>\nint main(){return 0;}"),
        "got: {}",
        out
    );
}

#[test]
fn java_text_block_preserved() {
    let input = "```java\nString s = \"\"\"\nhello\n\"\"\";\n```\n";
    let (out, w) = render_llm(input);
    assert!(w.is_empty(), "{:?}", w);
    assert!(out.contains("\"\"\"\nhello\n\"\"\""), "got: {}", out);
}

#[test]
fn sql_minified() {
    let input = "```sql\n-- list\nSELECT *\nFROM users;\n```\n";
    let (out, w) = render_llm(input);
    assert!(w.is_empty(), "{:?}", w);
    assert!(out.contains("SELECT*FROM users;"), "got: {}", out);
}

#[test]
fn keep_comments_preserves_inline() {
    let input = "```rust @minify-keep-comments\nfn x() {\n    // hi\n    1\n}\n```\n";
    let (out, w) = render_llm(input);
    assert!(out.contains("/* hi*/"), "comment kept: {}", out);
    assert!(
        w.iter().any(|s| s.contains("B0703")),
        "B0703 warning: {:?}",
        w
    );
}

#[test]
fn refused_python_with_minify_attr() {
    let input = "```python @minify\ndef f():\n    return 1\n```\n";
    let (out, w) = render_llm(input);
    assert!(out.contains("def f():\n    return 1"), "verbatim: {}", out);
    assert!(
        w.iter().any(|s| s.contains("B0704")),
        "B0704 emitted: {:?}",
        w
    );
}

#[test]
fn refused_yaml_silent_without_optin() {
    // YAML in default allowlist is excluded, no @minify, so no warning.
    let input = "```yaml\nkey: value\n```\n";
    let (out, w) = render_llm(input);
    assert!(out.contains("key: value"));
    assert!(w.is_empty(), "{:?}", w);
}

#[test]
fn nominify_overrides_minify_languages() {
    let input = "```rust @nominify\nfn x() {\n    1\n}\n```\n";
    let (out, w) = render_llm(input);
    assert!(w.is_empty(), "{:?}", w);
    assert!(
        out.contains("fn x() {"),
        "verbatim under @nominify: {}",
        out
    );
}

#[test]
fn long_block_emits_b0702() {
    let mut body = String::from("[\n");
    for i in 0..60 {
        body.push_str(&format!("  {}", i));
        if i < 59 {
            body.push(',');
        }
        body.push('\n');
    }
    body.push(']');
    let input = format!("```json\n{}\n```\n", body);
    let (_out, w) = render_llm(&input);
    assert!(
        w.iter().any(|s| s.contains("B0702")),
        "B0702 emitted: {:?}",
        w
    );
}

#[test]
fn typescript_alias() {
    let input = "```ts\nfunction f(x: number): string { return String(x); }\n```\n";
    let (out, w) = render_llm(input);
    assert!(w.is_empty(), "{:?}", w);
    assert!(
        out.contains("function f(x:number):string{return String(x);}"),
        "got: {}",
        out
    );
}

#[test]
fn go_block_preserves_asi_safe_newline() {
    let input = "```go\nfunc f() int {\n    return 1\n}\n```\n";
    let (out, w) = render_llm(input);
    assert!(w.is_empty(), "{:?}", w);
    assert!(out.contains("func f()int{\nreturn 1\n}"), "got: {}", out);
}

#[test]
fn multi_language_document() {
    let input =
        "# Doc\n\n```rust\nfn x() { 1 }\n```\n\n```sql\nSELECT 1;\n```\n\n```js\nlet x = 1;\n```\n";
    let (out, w) = render_llm(input);
    assert!(w.is_empty(), "{:?}", w);
    assert!(out.contains("fn x(){1}"), "rust ok: {}", out);
    assert!(out.contains("SELECT 1;"), "sql ok: {}", out);
    assert!(out.contains("let x=1;"), "js ok: {}", out);
}
