//! SoundFont (.sf2) ベースの sample render path。
//!
//! 設計詳細は docs/design/07-soundfont.md。
//! - `render_soundfont_note` — 1 ノート用 PoC ヘルパ (Phase 1)
//! - `SoundFontParams` / `resolve_soundfont_path` — params 解釈 + path 解決 (Phase 2)
//! - track 全体の render は `render/mod.rs` 側で `Synthesizer` を再利用する

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use serde_json::{Map, Value};
use thiserror::Error;

/// `Instrument::SoundFont` の params (`file` / `preset` / `bank`)。
///
/// `from_params` で JSON Map から取り出し、 `validate` で各値の妥当性を検査する。
#[derive(Debug, Clone)]
pub struct SoundFontParams {
    pub file: String,
    pub preset: u16,
    pub bank: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundFontParamsError {
    MissingFile,
    EmptyFile,
    InvalidPreset(String),
    InvalidBank(String),
}

impl std::fmt::Display for SoundFontParamsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingFile => write!(f, "soundfont params.file is required"),
            Self::EmptyFile => write!(f, "soundfont params.file must be non-empty"),
            Self::InvalidPreset(s) => write!(f, "soundfont params.preset must be 0..=127, got {s}"),
            Self::InvalidBank(s) => write!(f, "soundfont params.bank must be 0..=128, got {s}"),
        }
    }
}

impl SoundFontParams {
    /// 既定の bank。 GM/GS 互換 SF2 で melodic 音色に使う。
    pub const DEFAULT_BANK: u16 = 0;
    /// 既定の preset。 GM Program 0 = Acoustic Grand Piano。
    pub const DEFAULT_PRESET: u16 = 0;

    pub fn from_params(params: &Map<String, Value>) -> Result<Self, SoundFontParamsError> {
        let file = params
            .get("file")
            .ok_or(SoundFontParamsError::MissingFile)?
            .as_str()
            .ok_or(SoundFontParamsError::MissingFile)?
            .to_string();
        if file.is_empty() {
            return Err(SoundFontParamsError::EmptyFile);
        }

        let preset = match params.get("preset") {
            None => Self::DEFAULT_PRESET,
            Some(v) => {
                let n = v
                    .as_u64()
                    .ok_or_else(|| SoundFontParamsError::InvalidPreset(v.to_string()))?;
                if n > 127 {
                    return Err(SoundFontParamsError::InvalidPreset(n.to_string()));
                }
                n as u16
            }
        };

        let bank = match params.get("bank") {
            None => Self::DEFAULT_BANK,
            Some(v) => {
                let n = v
                    .as_u64()
                    .ok_or_else(|| SoundFontParamsError::InvalidBank(v.to_string()))?;
                if n > 128 {
                    return Err(SoundFontParamsError::InvalidBank(n.to_string()));
                }
                n as u16
            }
        };

        Ok(Self { file, preset, bank })
    }
}

/// `$CODETTA_SOUNDFONT_DIR` (default: `$HOME/Music/sf2/`) で SF2 path を解決する。
///
/// - 絶対 path: そのまま (env 無関係)
/// - 相対 path: `$CODETTA_SOUNDFONT_DIR/<file>` (env 未設定なら `$HOME/Music/sf2/<file>`)
/// - `$HOME` も取れなければ相対 path をそのまま返す (CWD 基準扱い)
pub fn resolve_soundfont_path(file: impl AsRef<Path>) -> PathBuf {
    let file = file.as_ref();
    if file.is_absolute() {
        return file.to_path_buf();
    }
    if let Ok(dir) = std::env::var("CODETTA_SOUNDFONT_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join(file);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join("Music").join("sf2").join(file);
        }
    }
    file.to_path_buf()
}

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

/// track 全体 (= 複数 note) を 1 つの `Synthesizer` で render するための入力。
///
/// `notes` は `start_sample` 昇順で渡す前提 (呼び出し側で sort 済み)。
/// `total_samples` は track 全体のサンプル長 (release tail まで含む)。
#[derive(Debug, Clone)]
pub struct SoundFontTrackRender {
    pub sf2_path: PathBuf,
    pub preset: u16,
    pub bank: u16,
    pub sample_rate: u32,
    pub total_samples: usize,
    pub notes: Vec<SoundFontTrackNote>,
}

#[derive(Debug, Clone, Copy)]
pub struct SoundFontTrackNote {
    /// note_on 発火時刻 (サンプル単位)
    pub start_sample: usize,
    /// note_off 発火時刻 (サンプル単位、 `start_sample` より大きい)
    pub end_sample: usize,
    pub midi_key: u8,
    pub velocity: u8,
}

/// SF2 track を 1 度の `Synthesizer` で render する。
///
/// rustysynth の `Synthesizer` は内部に channel/voice state を持つため per-voice 独立 closure
/// にはできない。 ここで note_on / note_off / render を時刻順に駆動し、 末尾の release tail も
/// `total_samples` まで render し切る。
pub fn render_soundfont_track(
    cfg: &SoundFontTrackRender,
) -> Result<StereoBuffer, SoundFontError> {
    let mut synth = open_synth(&cfg.sf2_path, cfg.sample_rate, cfg.bank, cfg.preset)?;

    let channel: i32 = 0;
    let mut left = vec![0.0f32; cfg.total_samples];
    let mut right = vec![0.0f32; cfg.total_samples];

    // event = (sample_idx, on/off, key, velocity)。 単純に on / off を 2 イベントに展開し、
    // sample_idx 昇順 + 同時刻なら off → on の順 (= 同 sample で連続するノートが切れずに渡る)。
    enum Ev {
        Off { key: u8 },
        On { key: u8, vel: u8 },
    }
    let mut events: Vec<(usize, u8, Ev)> = Vec::with_capacity(cfg.notes.len() * 2);
    for n in &cfg.notes {
        events.push((n.start_sample, 1, Ev::On { key: n.midi_key, vel: n.velocity }));
        events.push((n.end_sample, 0, Ev::Off { key: n.midi_key }));
    }
    events.sort_by_key(|(s, ord, _)| (*s, *ord));

    let mut cursor: usize = 0;
    for (sample_idx, _, ev) in events {
        let target = sample_idx.min(cfg.total_samples);
        if target > cursor {
            synth.render(&mut left[cursor..target], &mut right[cursor..target]);
            cursor = target;
        }
        match ev {
            Ev::On { key, vel } => synth.note_on(channel, key as i32, vel as i32),
            Ev::Off { key } => synth.note_off(channel, key as i32),
        }
    }

    if cursor < cfg.total_samples {
        synth.render(&mut left[cursor..], &mut right[cursor..]);
    }

    Ok(StereoBuffer { left, right })
}

fn open_synth(
    sf2_path: &Path,
    sample_rate: u32,
    bank: u16,
    preset: u16,
) -> Result<Synthesizer, SoundFontError> {
    if !sf2_path.exists() {
        return Err(SoundFontError::NotFound(sf2_path.to_path_buf()));
    }

    let mut file = File::open(sf2_path).map_err(|e| SoundFontError::Io {
        path: sf2_path.to_path_buf(),
        source: e,
    })?;

    let sound_font = SoundFont::new(&mut file).map_err(|e| SoundFontError::Parse {
        path: sf2_path.to_path_buf(),
        message: format!("{:?}", e),
    })?;
    let sound_font = Arc::new(sound_font);

    let settings = SynthesizerSettings::new(sample_rate as i32);
    let mut synth = Synthesizer::new(&sound_font, &settings)
        .map_err(|e| SoundFontError::Synth(format!("{:?}", e)))?;

    let channel: i32 = 0;
    // Bank select (CC0 = bank MSB, CC32 = LSB) + Program change。 GM/GS では bank 0 + program 0-127。
    synth.process_midi_message(channel, 0xB0, 0, bank as i32);
    synth.process_midi_message(channel, 0xB0, 32, 0);
    synth.process_midi_message(channel, 0xC0, preset as i32, 0);

    Ok(synth)
}

/// SF2 から 1 ノートを stereo PCM として render する PoC。
///
/// 呼び出し側で SF2 file path は絶対 path に解決済みである前提
/// (`$CODETTA_SOUNDFONT_DIR` の展開や `~` 展開は Phase 2 で render dispatch に組み込む)。
pub fn render_soundfont_note(
    params: &SoundFontRenderParams,
) -> Result<StereoBuffer, SoundFontError> {
    let mut synth = open_synth(&params.sf2_path, params.sample_rate, params.bank, params.preset)?;
    let channel: i32 = 0;

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
    use serde_json::json;

    fn sf2_from_env() -> Option<PathBuf> {
        std::env::var("CODETTA_TEST_SF2").ok().map(PathBuf::from)
    }

    #[test]
    fn params_required_file() {
        let m = Map::new();
        assert_eq!(
            SoundFontParams::from_params(&m).unwrap_err(),
            SoundFontParamsError::MissingFile
        );
    }

    #[test]
    fn params_rejects_empty_file() {
        let mut m = Map::new();
        m.insert("file".into(), json!(""));
        assert_eq!(
            SoundFontParams::from_params(&m).unwrap_err(),
            SoundFontParamsError::EmptyFile
        );
    }

    #[test]
    fn params_default_preset_bank() {
        let mut m = Map::new();
        m.insert("file".into(), json!("foo.sf2"));
        let p = SoundFontParams::from_params(&m).unwrap();
        assert_eq!(p.file, "foo.sf2");
        assert_eq!(p.preset, 0);
        assert_eq!(p.bank, 0);
    }

    #[test]
    fn params_preset_range_check() {
        let mut m = Map::new();
        m.insert("file".into(), json!("foo.sf2"));
        m.insert("preset".into(), json!(200));
        assert!(matches!(
            SoundFontParams::from_params(&m).unwrap_err(),
            SoundFontParamsError::InvalidPreset(_)
        ));
    }

    #[test]
    fn resolve_absolute_path_untouched() {
        let p = resolve_soundfont_path("/abs/path/x.sf2");
        assert_eq!(p, PathBuf::from("/abs/path/x.sf2"));
    }

    #[test]
    fn resolve_relative_uses_env() {
        let saved = std::env::var("CODETTA_SOUNDFONT_DIR").ok();
        std::env::set_var("CODETTA_SOUNDFONT_DIR", "/custom/sf2");
        let p = resolve_soundfont_path("piano.sf2");
        assert_eq!(p, PathBuf::from("/custom/sf2/piano.sf2"));
        match saved {
            Some(v) => std::env::set_var("CODETTA_SOUNDFONT_DIR", v),
            None => std::env::remove_var("CODETTA_SOUNDFONT_DIR"),
        }
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

    #[test]
    fn track_renders_multiple_notes_when_sf2_available() {
        let Some(sf2) = sf2_from_env() else {
            eprintln!("CODETTA_TEST_SF2 not set — skipping SF2 track render test");
            return;
        };

        // 0.0s / 0.5s / 1.0s に C4 / E4 / G4 を打鍵 (各 0.4s ホールド)
        let sr = 44100u32;
        let total = (sr as f32 * 2.0) as usize;
        let notes = vec![
            SoundFontTrackNote {
                start_sample: 0,
                end_sample: (sr as f32 * 0.4) as usize,
                midi_key: 60,
                velocity: 100,
            },
            SoundFontTrackNote {
                start_sample: (sr as f32 * 0.5) as usize,
                end_sample: (sr as f32 * 0.9) as usize,
                midi_key: 64,
                velocity: 100,
            },
            SoundFontTrackNote {
                start_sample: (sr as f32 * 1.0) as usize,
                end_sample: (sr as f32 * 1.4) as usize,
                midi_key: 67,
                velocity: 100,
            },
        ];
        let cfg = SoundFontTrackRender {
            sf2_path: sf2,
            preset: 0,
            bank: 0,
            sample_rate: sr,
            total_samples: total,
            notes,
        };
        let buf = render_soundfont_track(&cfg).expect("SF2 track render");
        assert_eq!(buf.left.len(), total);
        assert_eq!(buf.right.len(), total);
        let peak = buf
            .left
            .iter()
            .chain(buf.right.iter())
            .fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(peak > 0.01, "track peak too low: {peak}");
    }
}
