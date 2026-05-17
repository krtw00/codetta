//! Song を WAV へレンダリングするパイプライン。
//!
//! 信号フロー:
//!
//! ```text
//!   SF2 note_on → synth.render → × volume → pan → master mix → soft limiter → WAV
//! ```
//!
//! schema 0.2 / SF2 一本化:
//!
//! - 楽器: `soundfont` 一種のみ (= `Instrument::SoundFont`)。 内蔵 synth は CDT-7 で削除済
//! - サンプルレート: 44.1kHz / ビット深度: 16bit / stereo
//! - エフェクト: `lowpass` / `highpass` / `distortion` / `delay` / `reverb` を track.fx として適用 (順序通り)
//! - `--from` / `--to` トリミング: 未対応 (CLI 側で beat → samples 変換時に slice)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hound::{SampleFormat, WavSpec, WavWriter};
use rustysynth::SoundFont;

use crate::effect;
use crate::error::CodettaError;
use crate::model::{Effect, Pitch, Song};
use crate::synth::soundfont::{
    drum_key_to_midi, load_soundfont, render_soundfont_track_with, resolve_soundfont_path,
    SoundFontParams, SoundFontTrackNote, SoundFontTrackRender, DRUM_BANK,
};
use crate::synth::SAMPLE_RATE;

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
///
/// 信号フロー: 各トラックを mono voice → stereo (pan + volume) で per-track buffer に書き出し →
/// `track.fx` を順次適用 → master に加算 → soft limiter。
pub fn render_to_buffer(song: &Song) -> Vec<(f32, f32)> {
    let sr = SAMPLE_RATE as f32;
    let bpm = song.metadata.bpm.max(1) as f32;
    let sec_per_beat = 60.0 / bpm;

    // 末尾に 2 秒の余韻 (release tail のため)
    let total_sec = song.duration_beats() * sec_per_beat + 2.0;
    let total_samples = (total_sec * sr).ceil() as usize;
    let mut master = vec![(0.0_f32, 0.0_f32); total_samples];

    // 同一 SF2 を複数 track で参照する場合の load 重複回避用 cache。
    // key は `resolve_soundfont_path` 後の絶対 / 相対 path。 異なる文字列で同じ
    // ファイルを指していた場合は cache miss するが、 動作は変わらず optimization の損だけ。
    let mut sf2_cache: HashMap<PathBuf, Arc<SoundFont>> = HashMap::new();

    for track in &song.tracks {
        if track.mute {
            continue;
        }
        let kind = track.instrument.kind.as_str();
        let (gain_l, gain_r) = pan_gains(track.pan);
        let vol = track.volume;

        // per-track buffer に書き出し
        let mut track_buf = vec![(0.0_f32, 0.0_f32); total_samples];

        // schema 0.2 では `instrument.type` は "soundfont" のみ。 他 type は validate で
        // 弾かれる前提だが、 in-memory で組まれた未知 type もここで silent skip する。
        if kind != "soundfont" {
            continue;
        }

        // SF2 は per-voice 独立 render ができないので track 単位で Synthesizer を生成し、
        // note_on/note_off/render を時刻順に駆動して stereo PCM を取り出す。
        let sf_params = match SoundFontParams::from_params(&track.instrument.params) {
            Ok(p) => p,
            Err(_) => continue, // validate 側で報告済み
        };
        let resolved = resolve_soundfont_path(&sf_params.file);
        let sound_font = match sf2_cache.get(&resolved) {
            Some(sf) => sf.clone(),
            None => match load_soundfont(&resolved) {
                Ok(sf) => {
                    sf2_cache.insert(resolved.clone(), sf.clone());
                    sf
                }
                Err(e) => {
                    eprintln!(
                        "warning: soundfont track {:?} skipped due to load error: {}",
                        track.id, e
                    );
                    continue;
                }
            },
        };
        // bank=128 = GM Drum kit。 pitch が drum 要素名キー (kick 等) なら GM MIDI 番号に
        // 正規化し、 通常のノート名 / MIDI 番号との混在も受け付ける (02-project-format.md L175)。
        let is_drum_track = sf_params.bank == DRUM_BANK;
        let mut notes: Vec<SoundFontTrackNote> = track
            .notes
            .iter()
            .filter_map(|n| {
                let midi = match (is_drum_track, &n.pitch) {
                    (true, Pitch::Name(s)) => {
                        drum_key_to_midi(s).or_else(|| n.pitch.as_midi().ok())?
                    }
                    _ => n.pitch.as_midi().ok()?,
                };
                let start = (n.t * sec_per_beat * sr) as usize;
                let end_beat = n.t + n.dur;
                let end = (end_beat * sec_per_beat * sr) as usize;
                if end <= start {
                    return None;
                }
                Some(SoundFontTrackNote {
                    start_sample: start,
                    end_sample: end,
                    midi_key: midi,
                    velocity: n.vel,
                })
            })
            .collect();
        notes.sort_by_key(|n| n.start_sample);

        let cfg = SoundFontTrackRender {
            sf2_path: resolved,
            preset: sf_params.preset,
            bank: sf_params.bank,
            sample_rate: SAMPLE_RATE,
            total_samples,
            notes,
        };
        match render_soundfont_track_with(&cfg, sound_font) {
            Ok(stereo) => {
                // velocity は SF2 内部で note_on velocity として既に効いている。
                // 残るのは track.volume と pan。 pan は左右 gain として乗算。
                for (i, (sl, sr)) in stereo.left.iter().zip(stereo.right.iter()).enumerate() {
                    if i >= track_buf.len() {
                        break;
                    }
                    track_buf[i].0 += sl * vol * gain_l;
                    track_buf[i].1 += sr * vol * gain_r;
                }
            }
            Err(e) => {
                // 致命ではない: validate でも検出されるべきだが、 ここでは黙ってスキップせず
                // stderr に出す (CLI quiet/verbose の判定は呼び出し側にない)。
                eprintln!(
                    "warning: soundfont track {:?} skipped due to render error: {}",
                    track.id, e
                );
            }
        }

        // fx chain (順序通り適用)
        for fx in &track.fx {
            apply_effect(&mut track_buf, fx, song.metadata.bpm);
        }

        // master に加算
        for (m, t) in master.iter_mut().zip(track_buf.iter()) {
            m.0 += t.0;
            m.1 += t.1;
        }
    }

    // master gain (soft_clip 前) + ソフトリミッタ (tanh)
    let mg = song.metadata.master_gain;
    for (l, r) in &mut master {
        *l = soft_clip(*l * mg);
        *r = soft_clip(*r * mg);
    }
    master
}

/// `Effect` を 1 つ、 in-place で適用する。 未実装 type (現状 `reverb`) は黙ってスキップ。
fn apply_effect(buf: &mut [(f32, f32)], fx: &Effect, bpm: u32) {
    match fx.kind.as_str() {
        "lowpass" => {
            let cutoff = fx
                .params
                .get("cutoff")
                .and_then(|v| v.as_f64())
                .unwrap_or(1000.0) as f32;
            let q = fx.params.get("q").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            effect::lowpass(buf, cutoff, q, SAMPLE_RATE);
        }
        "highpass" => {
            let cutoff = fx
                .params
                .get("cutoff")
                .and_then(|v| v.as_f64())
                .unwrap_or(1000.0) as f32;
            let q = fx.params.get("q").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            effect::highpass(buf, cutoff, q, SAMPLE_RATE);
        }
        "distortion" => {
            let amount = fx
                .params
                .get("amount")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.3) as f32;
            let tone = fx
                .params
                .get("tone")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5) as f32;
            effect::distortion(buf, amount, tone, SAMPLE_RATE);
        }
        "delay" => {
            let default_time = serde_json::json!("1/8");
            let time_spec = fx.params.get("time").unwrap_or(&default_time);
            let time_sec = effect::parse_delay_time(time_spec, bpm);
            let feedback = fx
                .params
                .get("feedback")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.3) as f32;
            let mix = fx
                .params
                .get("mix")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.25) as f32;
            effect::delay(buf, time_sec, feedback, mix, SAMPLE_RATE);
        }
        "reverb" => {
            let size = fx
                .params
                .get("size")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5) as f32;
            let damp = fx
                .params
                .get("damp")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5) as f32;
            let mix = fx.params.get("mix").and_then(|v| v.as_f64()).unwrap_or(0.2) as f32;
            effect::reverb(buf, size, damp, mix, SAMPLE_RATE);
        }
        // 未知の type は validate で検出される。
        _ => {}
    }
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

    /// SF2 (= bank 0 / preset 0 Acoustic Grand Piano) を使った 1 ノート曲。 audible 検証用 test は
    /// `with_sf2!()` で env を要求して skip / 続行を判定する。
    fn one_note_song(sf2: &str) -> Song {
        let mut s = Song::new("smoke", 120, None);
        let mut inst = Instrument::new("soundfont");
        inst.params.insert("file".into(), serde_json::json!(sf2));
        inst.params.insert("preset".into(), serde_json::json!(0));
        inst.params.insert("bank".into(), serde_json::json!(0));
        s.tracks.push(Track {
            id: "lead".into(),
            name: "Lead".into(),
            instrument: inst,
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

    /// CODETTA_TEST_SF2 が無ければ早期 return する。 audible 検証は SF2 がローカルに無いと
    /// できないため、 CI 等の SF2 未配備環境ではテスト skip。
    macro_rules! with_sf2 {
        ($var:ident) => {
            let Some($var) = std::env::var("CODETTA_TEST_SF2").ok() else {
                eprintln!("CODETTA_TEST_SF2 not set — skipping audible SF2 render test");
                return;
            };
        };
    }

    #[test]
    fn buffer_has_audio() {
        with_sf2!(sf2);
        let buf = render_to_buffer(&one_note_song(&sf2));
        assert!(!buf.is_empty());
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert!(peak > 0.01, "expected audible peak, got {peak}");
    }

    #[test]
    fn master_gain_amplifies_output() {
        with_sf2!(sf2);
        let mut s = one_note_song(&sf2);
        s.tracks[0].volume = 0.2;
        let dry = render_to_buffer(&s);
        s.metadata.master_gain = 3.0;
        let wet = render_to_buffer(&s);
        let peak_dry = dry.iter().map(|(l, _)| l.abs()).fold(0.0_f32, f32::max);
        let peak_wet = wet.iter().map(|(l, _)| l.abs()).fold(0.0_f32, f32::max);
        assert!(
            peak_wet > peak_dry * 1.5,
            "master_gain=3.0 should noticeably raise peak: dry={peak_dry} wet={peak_wet}"
        );
    }

    #[test]
    fn master_gain_zero_silences_output() {
        with_sf2!(sf2);
        let mut s = one_note_song(&sf2);
        s.metadata.master_gain = 0.0;
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert_eq!(peak, 0.0);
    }

    #[test]
    fn mute_silences_track() {
        with_sf2!(sf2);
        let mut s = one_note_song(&sf2);
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
        // 未知 instrument type は無音で skip (validate 側でエラー出力されるが render は黙る)。
        // SF2 は不要 (内蔵 synth 削除済の schema 0.2 では soundfont 以外は全て silent)。
        let mut s = Song::new("smoke", 120, None);
        s.tracks.push(Track {
            id: "x".into(),
            name: "X".into(),
            instrument: Instrument::new("unknown_synth"),
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
    fn lowpass_fx_attenuates_high_freq_track() {
        with_sf2!(sf2);
        // bright preset (Saw Lead) に 500Hz lowpass を被せると RMS が下がる
        let mut s = one_note_song(&sf2);
        s.tracks[0]
            .instrument
            .params
            .insert("preset".into(), serde_json::json!(81));
        let dry = render_to_buffer(&s);
        s.tracks[0].fx.push(crate::model::Effect {
            kind: "lowpass".into(),
            params: {
                let mut m = serde_json::Map::new();
                m.insert("cutoff".into(), serde_json::json!(500.0));
                m.insert("q".into(), serde_json::json!(0.7));
                m
            },
        });
        let wet = render_to_buffer(&s);
        let rms_dry: f32 = (dry.iter().map(|(l, _)| l * l).sum::<f32>() / dry.len() as f32).sqrt();
        let rms_wet: f32 = (wet.iter().map(|(l, _)| l * l).sum::<f32>() / wet.len() as f32).sqrt();
        assert!(
            rms_wet < rms_dry * 0.97,
            "lowpass should attenuate harmonics: dry={rms_dry} wet={rms_wet}"
        );
        assert!(
            rms_wet > 1e-5,
            "lowpass should not silence the track: {rms_wet}"
        );
    }

    #[test]
    fn distortion_fx_changes_waveform() {
        with_sf2!(sf2);
        let mut s = one_note_song(&sf2);
        s.tracks[0]
            .instrument
            .params
            .insert("preset".into(), serde_json::json!(81));
        s.tracks[0].volume = 0.4;
        let dry = render_to_buffer(&s);
        s.tracks[0].fx.push(crate::model::Effect {
            kind: "distortion".into(),
            params: {
                let mut m = serde_json::Map::new();
                m.insert("amount".into(), serde_json::json!(1.0));
                m.insert("tone".into(), serde_json::json!(0.5));
                m
            },
        });
        let wet = render_to_buffer(&s);
        let diff_count = dry
            .iter()
            .zip(wet.iter())
            .filter(|(d, w)| (d.0 - w.0).abs() > 0.01)
            .count();
        assert!(
            diff_count > 100,
            "distortion should change samples: diff_count={diff_count}"
        );
    }

    #[test]
    fn delay_fx_extends_tail() {
        with_sf2!(sf2);
        let mut s = one_note_song(&sf2);
        s.tracks[0].fx.push(crate::model::Effect {
            kind: "delay".into(),
            params: {
                let mut m = serde_json::Map::new();
                m.insert("time".into(), serde_json::json!("1/8"));
                m.insert("feedback".into(), serde_json::json!(0.6));
                m.insert("mix".into(), serde_json::json!(0.5));
                m
            },
        });
        let buf = render_to_buffer(&s);
        let tail_start = (1.5 * SAMPLE_RATE as f32) as usize;
        if tail_start < buf.len() {
            let tail_rms: f32 = (buf[tail_start..].iter().map(|(l, _)| l * l).sum::<f32>()
                / (buf.len() - tail_start) as f32)
                .sqrt();
            assert!(
                tail_rms > 1e-5,
                "delay echo should leave audible tail: {tail_rms}"
            );
        }
    }

    #[test]
    fn reverb_fx_extends_tail() {
        with_sf2!(sf2);
        let mut s = one_note_song(&sf2);
        s.tracks[0].fx.push(crate::model::Effect {
            kind: "reverb".into(),
            params: {
                let mut m = serde_json::Map::new();
                m.insert("size".into(), serde_json::json!(0.7));
                m.insert("damp".into(), serde_json::json!(0.3));
                m.insert("mix".into(), serde_json::json!(0.6));
                m
            },
        });
        let buf = render_to_buffer(&s);
        let tail_start = buf.len().saturating_sub(SAMPLE_RATE as usize / 2);
        let tail_rms: f32 = (buf[tail_start..].iter().map(|(l, _)| l * l).sum::<f32>()
            / (buf.len() - tail_start) as f32)
            .sqrt();
        assert!(
            tail_rms > 1e-6,
            "reverb tail should leave audible energy: {tail_rms}"
        );
    }

    #[test]
    fn fx_chain_applied_in_order() {
        with_sf2!(sf2);
        let mut s_dl = one_note_song(&sf2);
        s_dl.tracks[0]
            .instrument
            .params
            .insert("preset".into(), serde_json::json!(81));
        // SF2 envelope は内蔵 synth より振幅が小さいので、 volume / master_gain を最大化して
        // distortion ⇆ lowpass の order 差が見える振幅を確保する。
        s_dl.tracks[0].volume = 1.0;
        s_dl.metadata.master_gain = 2.0;
        s_dl.tracks[0].notes[0].dur = 2.0;
        s_dl.tracks[0].fx = vec![
            crate::model::Effect {
                kind: "distortion".into(),
                params: {
                    let mut m = serde_json::Map::new();
                    m.insert("amount".into(), serde_json::json!(1.0));
                    m.insert("tone".into(), serde_json::json!(0.5));
                    m
                },
            },
            crate::model::Effect {
                kind: "lowpass".into(),
                params: {
                    let mut m = serde_json::Map::new();
                    m.insert("cutoff".into(), serde_json::json!(500.0));
                    m.insert("q".into(), serde_json::json!(0.7));
                    m
                },
            },
        ];
        let mut s_ld = s_dl.clone();
        s_ld.tracks[0].fx.swap(0, 1);
        let buf_dl = render_to_buffer(&s_dl);
        let buf_ld = render_to_buffer(&s_ld);
        // SF2 出力は小振幅でも distortion (tanh) の非線形性 + lowpass IIR で order 差は出る。
        // 閾値は内蔵 synth 時代の 0.001 から 1e-5 に緩めて SF2 振幅に追従。
        let diff: usize = buf_dl
            .iter()
            .zip(buf_ld.iter())
            .filter(|(a, b)| (a.0 - b.0).abs() > 1e-5)
            .count();
        assert!(diff > 100, "fx order should matter: diff={diff}");
    }

    #[test]
    fn soundfont_track_unknown_file_silently_skipped() {
        // file が存在しなくても render は落ちず無音で済む。 env 不要 (file が存在しないので
        // load を試みて失敗、 warning 出力後に continue する)。
        let mut s = Song::new("smoke", 120, None);
        let mut inst = Instrument::new("soundfont");
        inst.params.insert(
            "file".into(),
            serde_json::json!("/nonexistent/codetta-test/missing.sf2"),
        );
        s.tracks.push(Track {
            id: "x".into(),
            name: "X".into(),
            instrument: inst,
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
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert_eq!(peak, 0.0, "missing SF2 should leave silence (got {peak})");
    }

    #[test]
    fn sf2_drum_track_resolves_drum_key_to_midi() {
        // bank=128 + Pitch::Name("kick") が GM Drum MIDI 36 に正規化されることを、
        // 「audible な出力が出ること」 で確認する (CDT-5 で実装)。
        with_sf2!(sf2);
        let mut s = Song::new("drum-test", 120, None);
        let mut inst = Instrument::new("soundfont");
        inst.params.insert("file".into(), serde_json::json!(sf2));
        inst.params.insert("preset".into(), serde_json::json!(0));
        inst.params.insert("bank".into(), serde_json::json!(128));
        s.tracks.push(Track {
            id: "drums".into(),
            name: "Drums".into(),
            instrument: inst,
            volume: 0.8,
            pan: 0.0,
            mute: false,
            solo: false,
            fx: vec![],
            notes: vec![Note {
                t: 0.0,
                pitch: Pitch::Name("kick".into()),
                dur: 0.2,
                vel: 110,
            }],
        });
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert!(
            peak > 0.01,
            "drum 要素名キーは bank=128 で GM MIDI 36 (kick) に正規化されて audible: peak={peak}"
        );
    }

    #[test]
    fn soundfont_two_tracks_share_load_and_render_equivalently() {
        // 同一 SF2 を 2 つの track で参照したとき、 SF2 cache を経由しても
        // 「2 track の合算」 が「1 track 単体 × 2 の合算」 になることを確認する。
        with_sf2!(sf2);
        let mut inst = Instrument::new("soundfont");
        inst.params.insert("file".into(), serde_json::json!(sf2));
        inst.params.insert("preset".into(), serde_json::json!(0));

        let mut s_one = Song::new("one", 120, None);
        s_one.tracks.push(Track {
            id: "t1".into(),
            name: "T1".into(),
            instrument: inst.clone(),
            volume: 0.5,
            pan: 0.0,
            mute: false,
            solo: false,
            fx: vec![],
            notes: vec![Note {
                t: 0.0,
                pitch: Pitch::Name("A4".into()),
                dur: 1.0,
                vel: 100,
            }],
        });
        let buf_one = render_to_buffer(&s_one);

        let mut s_two = s_one.clone();
        s_two.tracks.push(s_two.tracks[0].clone());
        s_two.tracks[1].id = "t2".into();
        let buf_two = render_to_buffer(&s_two);

        assert_eq!(buf_one.len(), buf_two.len(), "duration should match");

        let peak_one = buf_one
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        let peak_two = buf_two
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert!(
            peak_two > peak_one,
            "two tracks should be louder than one: {peak_one} vs {peak_two}"
        );

        let mut total = 0usize;
        let mut agree = 0usize;
        for ((l1, _), (l2, _)) in buf_one.iter().zip(buf_two.iter()) {
            if l1.abs() < 1e-4 && l2.abs() < 1e-4 {
                continue;
            }
            total += 1;
            if l1.signum() == l2.signum() {
                agree += 1;
            }
        }
        assert!(
            total > 0 && (agree as f32 / total as f32) > 0.95,
            "two-track waveform sign should match: {agree}/{total}"
        );
    }

    #[test]
    fn writes_wav_file() {
        with_sf2!(sf2);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "codetta-render-test-{}-{}.wav",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            std::process::id()
        ));
        let stats = render_to_wav(&one_note_song(&sf2), &p).unwrap();
        assert_eq!(stats.sample_rate, 44100);
        assert_eq!(stats.bit_depth, 16);
        assert!(stats.duration_sec > 0.0);
        let meta = std::fs::metadata(&p).unwrap();
        assert!(meta.len() > 44, "WAV header + audio bytes expected");
        let _ = std::fs::remove_file(&p);
    }
}
