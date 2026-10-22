//! Code-block minifiers used by the LLM emit pass.
//!
//! v0.2 shipped JSON/JSONL. v0.3 adds Rust, Go, JavaScript/TypeScript, Java,
//! C/C++, and SQL via hand-rolled tokenizers. Each minifier is fail-closed:
//! any parse error returns `Err`, and the caller falls back to verbatim
//! emission with a B0701 warning.
//!
//! Minification is required to be semantically lossless — the broad rule is
//! `parse → minify → re-parse` produces structurally equal data. Where we
//! lack a real parser we settle for tokenizer-equivalence and a hand-curated
//! corpus of edge-case fixtures.

pub mod c_common;
pub mod c_cpp;
mod dispatch;
mod error;
pub mod go;
pub mod java;
pub mod javascript;
pub mod json;
mod options;
mod output;
pub mod rust;
pub mod sql;

pub use dispatch::{is_supported, minify, refusal_reason};
pub use error::MinifyError;
pub use options::MinifyOptions;
pub use output::{MinifyOutput, MinifyWarning};
