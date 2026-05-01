//! `brief-web` — static-site generator for Brief documents.
//!
//! See `crates/brief-web/README.md` (eventual) and the design proposal in the
//! repository root for the architectural overview. The split mirrors `mdBook`:
//! a `BookConfig` describes the project, a `Builder` produces a `dist/`
//! directory, and a development server with SSE-backed live reload runs
//! against the same builder.

pub mod builder;
pub mod config;
pub mod page;
pub mod scaffold;
pub mod server;
pub mod summary;
pub mod theme;
pub mod watch;

pub use builder::{BuildOutcome, Builder};
pub use config::{BookConfig, BuildConfig, ProjectConfig, ServerConfig, SiteConfig};
