//! Codetta Core
//!
//! 音楽プロジェクトのモデル、 DSP / シンセ、 WAV レンダリング、 MIDI I/O を提供する。
//! 副作用 (再生 / ネットワーク / UI) は持たず、 ファイル I/O のみ。
//!
//! Phase 0 first cut の現スコープ: モデル + JSON I/O + バリデーション + `sin`
//! オシレータでの WAV レンダリング。 他のオシレータ / フィルタ / ドラム / MIDI I/O は続く実装で。

pub mod edit;
pub mod effect;
pub mod error;
pub mod io;
pub mod model;
pub mod render;
pub mod synth;
pub mod validate;

pub use edit::{
    add_notes, add_track, clear_notes, edit_notes, remove_track, set_fx, set_instrument, set_notes,
    track_mut, EditNotesStats, NoteOp,
};
pub use error::{CodettaError, Severity, ValidationError};
pub use io::{load, save};
pub use model::{Effect, Instrument, Metadata, Note, Pitch, Song, Track};
pub use render::{render_to_buffer, render_to_wav, RenderStats};
pub use validate::{validate, KNOWN_DRUM_KEYS, KNOWN_EFFECT_TYPES, KNOWN_INSTRUMENT_TYPES};

/// 現バージョンで書き出すスキーマバージョン。
pub const SCHEMA_VERSION: &str = "0.1";

/// 読み込み可能なスキーマバージョン一覧。
pub const SUPPORTED_VERSIONS: &[&str] = &["0.1"];
