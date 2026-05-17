use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MidiError {
    #[error("file not found: {0}")]
    FileNotFound(PathBuf),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("MIDI parse failed: {0}")]
    Parse(String),

    /// SMF Type 2 (= 複数独立シーケンス) は ADR スコープ外。
    #[error("unsupported SMF format: {0:?}")]
    UnsupportedFormat(String),

    /// PPQ ベース以外 (= SMPTE timecode) は ADR スコープ外。
    #[error("unsupported MIDI timing (SMPTE timecode not supported)")]
    UnsupportedTiming,

    /// text-meta JSON が壊れている / schema 不正。
    /// caller (= import) は warning に格下げし MIDI のみ復元へ fallback する。
    #[error("invalid codetta extensions JSON: {0}")]
    InvalidExtensions(String),
}
