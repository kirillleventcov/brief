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
