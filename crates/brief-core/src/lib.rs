pub mod ast;
pub mod config;
pub mod convert;
pub mod diag;
pub mod emit;
pub mod fmt;
pub mod inline;
pub mod lexer;
pub mod minify;
pub mod parser;
pub mod project;
pub mod resolve;
pub mod shortcode;
pub mod span;
pub mod token;
pub mod validate;
#[cfg(feature = "watch")]
pub mod watch;

pub use diag::{Code, Diagnostic};
pub use span::{SourceMap, Span};
