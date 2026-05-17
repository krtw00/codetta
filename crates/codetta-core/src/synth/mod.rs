//! シンセエンジン。 schema 0.2 以降は SF2 一本化のため、 本モジュールは
//! `soundfont` サブモジュールと `SAMPLE_RATE` のみを公開する。 内蔵 synth
//! (`sin` / `saw` / `square` 等) は CDT-7 で削除済。

pub mod soundfont;

/// 標準サンプルレート。 44.1kHz 固定。
pub const SAMPLE_RATE: u32 = 44100;
