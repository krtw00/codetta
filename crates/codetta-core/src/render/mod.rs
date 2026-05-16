//! Song を WAV へレンダリングするパイプライン。
//!
//! 信号フロー (05-sound.md):
//!
//! ```text
//!   note → voice → × velocity → × volume → pan → master mix → soft limiter → WAV
//! ```
//!
//! Phase 0 first cut のサポート:
//!
//! - 楽器: `sin` のみ (他はスキップして無音)
//! - サンプルレート: 44.1kHz / ビット深度: 16bit / stereo
//! - エフェクト: 未実装 (track.fx は無視)
//! - `--from` / `--to` トリミング: 未対応 (CLI 側で beat → samples 変換時に slice)

use std::path::Path;

use hound::{SampleFormat, WavSpec, WavWriter};

use crate::error::CodettaError;
use crate::model::Song;
use crate::synth::{manual, midi_to_freq, AdsrParams, SAMPLE_RATE};

/// Song を WAV ファイルに書き出す。 出力時の経過秒数 (実時間) は返さない —
/// CLI 側で計測する。
pub fn render_to_wav(song: &Song, output: impl AsRef<Path>) -> Result<RenderStats, CodettaError> {
    let buf = render_to_buffer(song);
    let frames = buf.len();
    let duration_sec = frames as f32 / SAMPLE_RATE as f32;
    write_wav(&buf, output.as_ref())?;
    Ok(RenderStats {
        frames,
        duration_sec,
        sample_rate: SAMPLE_RATE,
        bit_depth: 16,
    })
}

/// レンダリング結果のメタ情報。 CLI が JSON 出力に使う。
#[derive(Debug, Clone)]
pub struct RenderStats {
    pub frames: usize,
    pub duration_sec: f32,
    pub sample_rate: u32,
    pub bit_depth: u16,
}

/// Song → ステレオ f32 サンプル列 (interleave なし、 `(L, R)` の Vec)。
pub fn render_to_buffer(song: &Song) -> Vec<(f32, f32)> {
    let sr = SAMPLE_RATE as f32;
    let bpm = song.metadata.bpm.max(1) as f32;
    let sec_per_beat = 60.0 / bpm;

    // 末尾に 2 秒の余韻 (release tail のため)
    let total_sec = song.duration_beats() * sec_per_beat + 2.0;
    let total_samples = (total_sec * sr).ceil() as usize;
    let mut master = vec![(0.0_f32, 0.0_f32); total_samples];

    for track in &song.tracks {
        if track.mute || track.instrument.kind != "sin" {
            continue;
        }
        let adsr = AdsrParams::from_params(&track.instrument.params);
        let (gain_l, gain_r) = pan_gains(track.pan);
        let vol = track.volume;

        for note in &track.notes {
            let midi = match note.pitch.as_midi() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let freq = midi_to_freq(midi);
            let start_sample = (note.t * sec_per_beat * sr) as usize;
            let hold_sec = note.dur * sec_per_beat;
            let voice = manual::render_voice(freq, hold_sec, adsr);
            let vel_gain = note.vel as f32 / 127.0;
            let g = vol * vel_gain;
            for (i, s) in voice.iter().enumerate() {
                let idx = start_sample + i;
                if idx >= master.len() {
                    break;
                }
                master[idx].0 += s * g * gain_l;
                master[idx].1 += s * g * gain_r;
            }
        }
    }

    // ソフトリミッタ (tanh)
    for (l, r) in &mut master {
        *l = soft_clip(*l);
        *r = soft_clip(*r);
    }
    master
}

/// equal-power (-3 dB center) パン。 `pan` は -1.0 (L) .. 1.0 (R)。
fn pan_gains(pan: f32) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0);
    let theta = (p + 1.0) * std::f32::consts::FRAC_PI_4; // 0..PI/2
    (theta.cos(), theta.sin())
}

/// tanh ベースのソフトクリッパ。 マスタリング目的ではなく、 加算による
/// 1.0 超過を抑える保険。 -0.5 dB 近辺で頭打ちになる。
fn soft_clip(x: f32) -> f32 {
    // 0.9 で割って戻すと、 |x|<<1 のときほぼ無加工。 大きい入力でも tanh が -1..1 に収める。
    (x * 0.9).tanh() / 0.9
}

fn write_wav(buf: &[(f32, f32)], path: &Path) -> Result<(), CodettaError> {
    let spec = WavSpec {
        channels: 2,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut wr = WavWriter::create(path, spec)?;
    for (l, r) in buf {
        let li = (l.clamp(-1.0, 1.0) * 32767.0) as i16;
        let ri = (r.clamp(-1.0, 1.0) * 32767.0) as i16;
        wr.write_sample(li)?;
        wr.write_sample(ri)?;
    }
    wr.finalize()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Instrument, Note, Pitch, Track};

    fn one_note_song() -> Song {
        let mut s = Song::new("smoke", 120, None);
        s.tracks.push(Track {
            id: "lead".into(),
            name: "Lead".into(),
            instrument: Instrument::new("sin"),
            volume: 0.8,
            pan: 0.0,
            mute: false,
            solo: false,
            fx: vec![],
            notes: vec![Note {
                t: 0.0,
                pitch: Pitch::Name("A4".into()),
                dur: 0.5,
                vel: 100,
            }],
        });
        s
    }

    #[test]
    fn buffer_has_audio() {
        let buf = render_to_buffer(&one_note_song());
        assert!(!buf.is_empty());
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert!(peak > 0.1, "expected audible peak, got {peak}");
    }

    #[test]
    fn mute_silences_track() {
        let mut s = one_note_song();
        s.tracks[0].mute = true;
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert_eq!(peak, 0.0);
    }

    #[test]
    fn unknown_instrument_silently_skipped() {
        let mut s = one_note_song();
        s.tracks[0].instrument = Instrument::new("saw_lead"); // Phase 0 first cut では未実装
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert_eq!(peak, 0.0);
    }

    #[test]
    fn pan_gains_center() {
        let (l, r) = pan_gains(0.0);
        assert!((l - r).abs() < 1e-6);
        // -3 dB → 約 0.707
        assert!((l - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
    }

    #[test]
    fn pan_gains_extremes() {
        let (l, r) = pan_gains(-1.0);
        assert!(l > 0.99 && r < 0.01);
        let (l, r) = pan_gains(1.0);
        assert!(r > 0.99 && l < 0.01);
    }

    #[test]
    fn writes_wav_file() {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "codetta-render-test-{}-{}.wav",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            std::process::id()
        ));
        let stats = render_to_wav(&one_note_song(), &p).unwrap();
        assert_eq!(stats.sample_rate, 44100);
        assert_eq!(stats.bit_depth, 16);
        assert!(stats.duration_sec > 0.0);
        // 実ファイルが書けているか
        let meta = std::fs::metadata(&p).unwrap();
        assert!(meta.len() > 44, "WAV header + audio bytes expected");
        let _ = std::fs::remove_file(&p);
    }
}
