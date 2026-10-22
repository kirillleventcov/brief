//! `book.toml`: the project configuration of a brief-web site.

use brief::shortcode::Shortcode;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Top-level `book.toml` configuration.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct BookConfig {
    #[serde(default)]
    pub book: ProjectConfig,
    #[serde(default)]
    pub build: BuildConfig,
    #[serde(default)]
    pub site: SiteConfig,
    #[serde(default)]
    pub server: ServerConfig,
    /// User-defined shortcodes, keyed by name, added to the built-ins.
    #[serde(default)]
    pub shortcodes: BTreeMap<String, Shortcode>,
}

/// The `[book]` table: metadata describing the site.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ProjectConfig {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub authors: Vec<String>,
}

/// The `[build]` table: where sources are read from and output is written,
/// both relative to the project directory.
#[derive(Clone, Debug, Deserialize)]
pub struct BuildConfig {
    #[serde(default = "default_src")]
    pub src: PathBuf,
    #[serde(default = "default_out")]
    pub out: PathBuf,
}

impl Default for BuildConfig {
    fn default() -> Self {
        BuildConfig {
            src: default_src(),
            out: default_out(),
        }
    }
}

fn default_src() -> PathBuf {
    PathBuf::from("src")
}
fn default_out() -> PathBuf {
    PathBuf::from("dist")
}

/// The `[site]` table: how the built site is deployed and themed.
#[derive(Clone, Debug, Deserialize)]
pub struct SiteConfig {
    /// URL prefix the site is served under; built-in asset URLs are rooted
    /// here.
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// Theme directory, relative to the project directory. Defaults to
    /// `theme/` when that exists.
    #[serde(default)]
    pub theme: Option<PathBuf>,
}

impl Default for SiteConfig {
    fn default() -> Self {
        SiteConfig {
            base_url: default_base_url(),
            theme: None,
        }
    }
}

fn default_base_url() -> String {
    "/".into()
}

/// The `[server]` table: where `brief-web serve` listens.
#[derive(Clone, Debug, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_host")]
    pub host: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            port: default_port(),
            host: default_host(),
        }
    }
}

fn default_port() -> u16 {
    3000
}
fn default_host() -> String {
    "127.0.0.1".into()
}

impl BookConfig {
    /// Read and parse `book.toml` from `project_dir`.
    ///
    /// Fails with a human-readable message if the file cannot be read or is
    /// not valid configuration.
    pub fn load_from_dir(project_dir: &Path) -> Result<Self, String> {
        let path = project_dir.join("book.toml");
        let text = fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
        let config: BookConfig =
            toml::from_str(&text).map_err(|e| format!("invalid {}: {}", path.display(), e))?;
        Ok(config)
    }
}
