//! SoundFont (.sf2) ベースの sample render path (PoC)。
//!
//! 設計詳細は docs/design/07-soundfont.md。 Phase 1 = PoC スコープ:
//! 「ファイル path + preset + MIDI key を渡して 1 ノートを stereo PCM として返す」
//! ところまで。 render dispatch / catalog / MCP server への統合は Phase 2 以降。

use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;

use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SoundFontError {
    #[error("SF2 file not found: {0}")]
    NotFound(PathBuf),

    #[error("I/O error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("SoundFont parse error ({path}): {message}")]
    Parse { path: PathBuf, message: String },

    #[error("Synthesizer init error: {0}")]
    Synth(String),
}

#[derive(Debug, Clone)]
pub struct SoundFontRenderParams {
    pub sf2_path: PathBuf,
    pub preset: u16,
    pub bank: u16,
    pub midi_key: u8,
    pub velocity: u8,
    pub hold_sec: f32,
    pub release_tail_sec: f32,
    pub sample_rate: u32,
}

#[derive(Debug, Clone)]
pub struct StereoBuffer {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

/// SF2 から 1 ノートを stereo PCM として render する PoC。
///
/// 呼び出し側で SF2 file path は絶対 path に解決済みである前提
/// (`$CODETTA_SOUNDFONT_DIR` の展開や `~` 展開は Phase 2 で render dispatch に組み込む)。
pub fn render_soundfont_note(
    params: &SoundFontRenderParams,
) -> Result<StereoBuffer, SoundFontError> {
    if !params.sf2_path.exists() {
        return Err(SoundFontError::NotFound(params.sf2_path.clone()));
    }

    let mut file = File::open(&params.sf2_path).map_err(|e| SoundFontError::Io {
        path: params.sf2_path.clone(),
        source: e,
    })?;

    let sound_font = SoundFont::new(&mut file).map_err(|e| SoundFontError::Parse {
        path: params.sf2_path.clone(),
        message: format!("{:?}", e),
    })?;
    let sound_font = Arc::new(sound_font);

    let settings = SynthesizerSettings::new(params.sample_rate as i32);
    let mut synth = Synthesizer::new(&sound_font, &settings)
        .map_err(|e| SoundFontError::Synth(format!("{:?}", e)))?;

    let channel: i32 = 0;
    // Bank select (MSB on CC 0, LSB on CC 32) + Program change。
    // GM/GS 互換 SF2 では bank 0 + preset N で 128 種の標準音色が選べる。
    synth.process_midi_message(channel, 0xB0, 0, params.bank as i32);
    synth.process_midi_message(channel, 0xB0, 32, 0);
    synth.process_midi_message(channel, 0xC0, params.preset as i32, 0);

    let hold_samples = (params.hold_sec * params.sample_rate as f32).round() as usize;
    let tail_samples = (params.release_tail_sec * params.sample_rate as f32).round() as usize;
    let total = hold_samples + tail_samples;

    let mut left = vec![0.0f32; total];
    let mut right = vec![0.0f32; total];

    synth.note_on(channel, params.midi_key as i32, params.velocity as i32);
    if hold_samples > 0 {
        synth.render(&mut left[..hold_samples], &mut right[..hold_samples]);
    }

    synth.note_off(channel, params.midi_key as i32);
    if tail_samples > 0 {
        synth.render(&mut left[hold_samples..], &mut right[hold_samples..]);
    }

    Ok(StereoBuffer { left, right })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sf2_from_env() -> Option<PathBuf> {
        std::env::var("CODETTA_TEST_SF2").ok().map(PathBuf::from)
    }

    #[test]
    fn missing_file_returns_not_found() {
        let params = SoundFontRenderParams {
            sf2_path: PathBuf::from("/nonexistent/codetta-test/missing.sf2"),
            preset: 0,
            bank: 0,
            midi_key: 60,
            velocity: 100,
            hold_sec: 0.1,
            release_tail_sec: 0.1,
            sample_rate: 44100,
        };
        let err = render_soundfont_note(&params).unwrap_err();
        assert!(matches!(err, SoundFontError::NotFound(_)));
    }

    #[test]
    fn renders_c4_when_sf2_available() {
        let Some(sf2) = sf2_from_env() else {
            eprintln!("CODETTA_TEST_SF2 not set — skipping (PoC SF2 render test)");
            return;
        };

        let params = SoundFontRenderParams {
            sf2_path: sf2,
            preset: 0, // GM Program 0 = Acoustic Grand Piano
            bank: 0,
            midi_key: 60, // C4
            velocity: 100,
            hold_sec: 1.0,
            release_tail_sec: 0.5,
            sample_rate: 44100,
        };

        let buf = render_soundfont_note(&params).expect("SF2 render should succeed");

        let expected_samples = 44100 + 22050;
        assert_eq!(buf.left.len(), expected_samples);
        assert_eq!(buf.right.len(), expected_samples);

        let peak = buf
            .left
            .iter()
            .chain(buf.right.iter())
            .fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(peak > 0.01, "rendered peak suspiciously low: {peak}");
        assert!(peak <= 2.0, "rendered peak suspiciously high: {peak}");

        let hold_peak = buf
            .left
            .iter()
            .take(44100)
            .fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(
            hold_peak > 0.01,
            "no signal during hold phase: peak={hold_peak}"
        );
    }
}
