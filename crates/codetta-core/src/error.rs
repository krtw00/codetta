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

    #[error("track not found: {0}")]
    TrackNotFound(String),

    #[error("duplicate track id: {0}")]
    TrackIdDuplicate(String),

    #[error("validation failed ({} error(s))", .0.len())]
    Validation(Vec<ValidationError>),

    #[error("WAV error: {0}")]
    Wav(#[from] hound::Error),

    #[error("render failed: {0}")]
    Render(String),
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ValidationError {
    pub code: &'static str,
    pub path: String,
    pub message: String,
    pub severity: Severity,
}

impl ValidationError {
    pub fn new(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
            severity: Severity::Error,
        }
    }

    pub fn warning(
        code: &'static str,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
            severity: Severity::Warning,
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self.severity, Severity::Error)
    }

    pub fn is_warning(&self) -> bool {
        matches!(self.severity, Severity::Warning)
    }
}
