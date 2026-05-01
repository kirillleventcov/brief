use brief::shortcode::Shortcode;
use serde::Deserialize;
use std::collections::BTreeMap;
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
    #[serde(default)]
    pub shortcodes: BTreeMap<String, Shortcode>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ProjectConfig {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub authors: Vec<String>,
}

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

#[derive(Clone, Debug, Deserialize)]
pub struct SiteConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
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
    pub fn load_from_dir(project_dir: &Path) -> Result<Self, String> {
        let path = project_dir.join("book.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
        let cfg: BookConfig =
            toml::from_str(&text).map_err(|e| format!("invalid {}: {}", path.display(), e))?;
        Ok(cfg)
    }
}
