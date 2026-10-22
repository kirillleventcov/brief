//! The error type returned when a code block cannot be minified.

use std::error::Error;
use std::fmt;

/// A minifier rejected its input (unterminated literal, invalid JSON, …) or
/// no minifier exists for the requested language.
///
/// The LLM emit pass reacts by emitting the block verbatim with a B0701
/// warning that includes [`MinifyError::message`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct MinifyError {
    /// Human-readable description of why minification failed.
    pub message: String,
}

impl MinifyError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        MinifyError {
            message: message.into(),
        }
    }
}

impl fmt::Display for MinifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for MinifyError {}
