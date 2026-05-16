use std::path::PathBuf;

use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodettaError {
    #[error("file not found: {0}")]
    FileNotFound(PathBuf),

    #[error("file already exists: {0}")]
    FileExists(PathBuf),

    #[error("invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("unknown schema version: {0}")]
    UnknownVersion(String),

    #[error("validation failed ({} error(s))", .0.len())]
    Validation(Vec<ValidationError>),

    #[error("WAV error: {0}")]
    Wav(#[from] hound::Error),

    #[error("render failed: {0}")]
    Render(String),
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ValidationError {
    pub code: &'static str,
    pub path: String,
    pub message: String,
}

impl ValidationError {
    pub fn new(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}
