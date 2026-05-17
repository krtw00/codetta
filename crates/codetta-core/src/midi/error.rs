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

    /// melodic track が 16 channel + drum (ch10) を超えた (= ADR L75)。
    /// payload は超過した track id (= 救済策の自動実行はせず、 ユーザーに対処を促す)。
    #[error(
        "track limit exceeded: only 15 melodic + 1 drum channels available; excess tracks: {0:?}"
    )]
    TrackLimitExceeded(Vec<String>),

    /// drum track (= `bank=128`) が複数定義されている (= ADR L74)。 ch10 は 1 本固定。
    /// payload は drum と判定された全 track id (= 1 本に統合 or 削除をユーザーに促す)。
    #[error("multiple drum tracks not supported (only one ch10 drum track allowed): {0:?}")]
    MultipleDrumTracksNotSupported(Vec<String>),

    /// note pitch が export 経路で MIDI 番号に変換できない (= melodic track に drum 要素名キー、 不明 note 名、 範囲外)。
    #[error("invalid note pitch on track {track_id:?}: {reason}")]
    InvalidNotePitch { track_id: String, reason: String },

    /// soundfont 以外の instrument type が来た (= schema 0.2 では `soundfont` 一本)。
    /// caller が事前に 0.1 → 0.2 implicit migrate しているはずなので、 ここに来るのは bug。
    #[error("unsupported instrument type {kind:?} on track {track_id:?}; schema 0.2 expects 'soundfont'")]
    UnsupportedInstrumentType { track_id: String, kind: String },

    /// soundfont params (file / preset / bank) が読めない (= export には preset/bank 値が必須)。
    #[error("invalid soundfont params on track {track_id:?}: {reason}")]
    InvalidSoundFontParams { track_id: String, reason: String },
}
