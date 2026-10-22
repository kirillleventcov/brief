//! `brief.toml`: the per-project configuration every tool loads.
//!
//! These are options structs: each implements `Default` (the value an
//! absent key takes), so code building one by hand can name only the fields
//! it cares about and fill the rest with `..Default::default()`.

use crate::shortcode::{Registry, Shortcode};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// A parsed `brief.toml`.
// Every config struct carries `deny_unknown_fields`: a typoed or
// unsupported key must be a load error, not a silent no-op — the same
// strictness the language itself promises.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub project: Project,
    #[serde(default)]
    pub compile: Compile,
    /// User-defined shortcodes, keyed by name; they extend the built-ins.
    #[serde(default)]
    pub shortcodes: BTreeMap<String, Shortcode>,
}

/// The `[project]` table: descriptive metadata only.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Project {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

/// The `[compile]` table: options that change what a compile produces.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Compile {
    /// Report a heading that skips a level (`#` straight to `###`).
    #[serde(default)]
    pub strict_heading_levels: bool,
    /// Target used when the command line names none: `html`, `llm` or
    /// `json`.
    #[serde(default)]
    pub default_target: Option<String>,
    #[serde(default)]
    pub llm: LlmCompile,
}

/// The `[compile.llm]` table: how the LLM target treats code blocks.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LlmCompile {
    /// Master switch. When false, code blocks are emitted verbatim regardless
    /// of language tag or `@minify` attribute.
    #[serde(default = "default_minify_code_blocks")]
    pub minify_code_blocks: bool,
    /// Allowlist of language tags eligible for minification. Tags are
    /// matched case-insensitively. Defaults to the v0.2 set.
    #[serde(default = "default_minify_languages")]
    pub minify_languages: Vec<String>,
    /// Whether to keep the surrounding ```lang fence around minified code.
    /// True (default) preserves the structural cue for the consumer LLM.
    #[serde(default = "default_preserve_code_fences")]
    pub preserve_code_fences: bool,
}

impl Default for LlmCompile {
    fn default() -> Self {
        LlmCompile {
            minify_code_blocks: default_minify_code_blocks(),
            minify_languages: default_minify_languages(),
            preserve_code_fences: default_preserve_code_fences(),
        }
    }
}

fn default_minify_code_blocks() -> bool {
    true
}
fn default_minify_languages() -> Vec<String> {
    // v0.3 ships minifiers for JSON/JSONL plus the C-family + SQL set.
    // Aliases are listed explicitly so a user pruning the list by tag name
    // doesn't accidentally lose a language.
    vec![
        "json".into(),
        "jsonl".into(),
        "rust".into(),
        "rs".into(),
        "c".into(),
        "h".into(),
        "cpp".into(),
        "c++".into(),
        "cc".into(),
        "cxx".into(),
        "hpp".into(),
        "hxx".into(),
        "java".into(),
        "go".into(),
        "javascript".into(),
        "js".into(),
        "typescript".into(),
        "ts".into(),
        "sql".into(),
    ]
}
fn default_preserve_code_fences() -> bool {
    true
}

/// Read and parse the config file at `path`.
///
/// # Errors
///
/// Returns the I/O or TOML error, rendered as a string, when the file
/// cannot be read or is not a valid `brief.toml` (unknown keys included).
pub fn load(path: &Path) -> Result<Config, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    toml::from_str(&text).map_err(|e| e.to_string())
}

/// The shortcode registry `config` defines: the built-ins plus every
/// shortcode declared under `[shortcodes]`.
#[must_use]
pub fn registry_from(config: &Config) -> Registry {
    let mut registry = Registry::with_builtins();
    registry.extend(config.shortcodes.clone());
    registry
}
