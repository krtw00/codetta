//! Codetta Core
//!
//! 音楽プロジェクトのモデル、 DSP / シンセ、 WAV レンダリング、 MIDI I/O を提供する。
//! 副作用 (再生 / ネットワーク / UI) は持たず、 ファイル I/O のみ。
//!
//! Phase 0 の現スコープ: モデル + JSON I/O + バリデーション。
//! 合成 / レンダリングは続くコミットで追加する。

pub mod error;
pub mod io;
pub mod model;
pub mod validate;

pub use error::{CodettaError, ValidationError};
pub use io::{load, save};
pub use model::{Effect, Instrument, Metadata, Note, Pitch, Song, Track};
pub use validate::{validate, KNOWN_DRUM_KEYS, KNOWN_EFFECT_TYPES, KNOWN_INSTRUMENT_TYPES};

/// 現バージョンで書き出すスキーマバージョン。
pub const SCHEMA_VERSION: &str = "0.1";

/// 読み込み可能なスキーマバージョン一覧。
pub const SUPPORTED_VERSIONS: &[&str] = &["0.1"];
