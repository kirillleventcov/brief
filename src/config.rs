use crate::shortcode::{Registry, Shortcode};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub project: Project,
    #[serde(default)]
    pub compile: Compile,
    #[serde(default)]
    pub shortcodes: BTreeMap<String, Shortcode>,
    #[serde(default)]
    pub hooks: Hooks,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Project {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Compile {
    #[serde(default)]
    pub strict_heading_levels: bool,
    #[serde(default)]
    pub default_target: Option<String>,
    #[serde(default)]
    pub llm: LlmCompile,
}

#[derive(Clone, Debug, Deserialize)]
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
    vec!["json".into(), "jsonl".into()]
}
fn default_preserve_code_fences() -> bool {
    true
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Hooks {
    #[serde(default)]
    pub before_compile: Vec<String>,
    #[serde(default)]
    pub after_compile: Vec<String>,
}

pub fn load(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    toml::from_str(&text).map_err(|e| e.to_string())
}

pub fn registry_from(cfg: &Config) -> Registry {
    let mut reg = Registry::with_builtins();
    reg.extend(cfg.shortcodes.clone());
    reg
}
