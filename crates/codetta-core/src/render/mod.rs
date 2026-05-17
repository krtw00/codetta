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
//! - 楽器: `sin` / `saw` / `saw_lead` / `square` / `square_bass` / `triangle` / `saw_pad` (他はスキップして無音)
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
use crate::model::{Effect, Song};
use crate::synth::soundfont::{
    load_soundfont, render_soundfont_track_with, resolve_soundfont_path, SoundFontParams,
    SoundFontTrackNote, SoundFontTrackRender,
};
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

        if kind == "soundfont" {
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
            let mut notes: Vec<SoundFontTrackNote> = track
                .notes
                .iter()
                .filter_map(|n| {
                    let midi = n.pitch.as_midi().ok()?;
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
        } else if kind == "drum_kit" {
            // drum は note.pitch.as_drum_key() で voice を分岐。 melodic 経路と違って
            // freq/hold を取らないので、 closure ではなくここで直接ループする。
            let kit = kit_from_params(&track.instrument.params);
            for note in &track.notes {
                let drum_key = match note.pitch.as_drum_key() {
                    Ok(k) => k,
                    Err(_) => continue,
                };
                let voice = match drum_key {
                    "kick" => manual::render_drum_kick(kit.as_deref()),
                    "snare" => manual::render_drum_snare(kit.as_deref()),
                    "hh_closed" => manual::render_drum_hh(false),
                    "hh_open" => manual::render_drum_hh(true),
                    "clap" => manual::render_drum_clap(),
                    "crash" => manual::render_drum_crash(),
                    "ride" => manual::render_drum_ride(),
                    "tom_lo" => manual::render_drum_tom(80.0),
                    "tom_mid" => manual::render_drum_tom(150.0),
                    "tom_hi" => manual::render_drum_tom(220.0),
                    _ => continue,
                };
                let start_sample = (note.t * sec_per_beat * sr) as usize;
                let vel_gain = note.vel as f32 / 127.0;
                let g = vol * vel_gain;
                for (i, s) in voice.iter().enumerate() {
                    let idx = start_sample + i;
                    if idx >= track_buf.len() {
                        break;
                    }
                    track_buf[idx].0 += s * g * gain_l;
                    track_buf[idx].1 += s * g * gain_r;
                }
            }
        } else {
            // melodic 楽器: 楽器ごとに extra params が違うので closure で受ける
            // (sin/saw は adsr のみ、 square は pulse_width 追加)。
            let adsr = AdsrParams::from_params(&track.instrument.params);
            let render_voice: Box<dyn Fn(f32, f32) -> Vec<f32>> = match kind {
                "sin" => Box::new(move |f, h| manual::render_voice(f, h, adsr)),
                "saw" | "saw_lead" => Box::new(move |f, h| manual::render_voice_saw(f, h, adsr)),
                "square" | "square_bass" => {
                    let pw = pulse_width_from_params(&track.instrument.params);
                    Box::new(move |f, h| manual::render_voice_square(f, h, adsr, pw))
                }
                "triangle" => Box::new(move |f, h| manual::render_voice_triangle(f, h, adsr)),
                "saw_pad" => {
                    let detune = detune_cents_from_params(&track.instrument.params);
                    Box::new(move |f, h| manual::render_voice_saw_pad(f, h, adsr, detune))
                }
                _ => continue, // 未知の type は validate で検出される
            };
            for note in &track.notes {
                let midi = match note.pitch.as_midi() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let freq = midi_to_freq(midi);
                let start_sample = (note.t * sec_per_beat * sr) as usize;
                let hold_sec = note.dur * sec_per_beat;
                let voice = render_voice(freq, hold_sec);
                let vel_gain = note.vel as f32 / 127.0;
                let g = vol * vel_gain;
                for (i, s) in voice.iter().enumerate() {
                    let idx = start_sample + i;
                    if idx >= track_buf.len() {
                        break;
                    }
                    track_buf[idx].0 += s * g * gain_l;
                    track_buf[idx].1 += s * g * gain_r;
                }
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

/// Instrument.params から pulse_width (square/pulse 用) を取り出す。 デフォルト 0.5、 範囲 0.05-0.95。
fn pulse_width_from_params(params: &serde_json::Map<String, serde_json::Value>) -> f32 {
    params
        .get("pulse_width")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32)
        .unwrap_or(0.5)
}

/// Instrument.params から detune_cents (saw_pad 用) を取り出す。 デフォルト 10 (sound.md)。
fn detune_cents_from_params(params: &serde_json::Map<String, serde_json::Value>) -> f32 {
    params
        .get("detune_cents")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32)
        .unwrap_or(10.0)
}

/// Instrument.params から kit (drum_kit 用) を取り出す。 未指定なら None (= default レシピ)。
fn kit_from_params(params: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    params
        .get("kit")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
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
    fn master_gain_amplifies_output() {
        // master_gain > 1.0 で peak が増える (soft_clip 内の線形領域に収まる範囲なら
        // 単純な乗算と等価)。 元の volume を小さく取って soft_clip 飽和を避ける。
        let mut s = one_note_song();
        s.tracks[0].volume = 0.2;
        let dry = render_to_buffer(&s);
        s.metadata.master_gain = 3.0;
        let wet = render_to_buffer(&s);
        let peak_dry = dry.iter().map(|(l, _)| l.abs()).fold(0.0_f32, f32::max);
        let peak_wet = wet.iter().map(|(l, _)| l.abs()).fold(0.0_f32, f32::max);
        assert!(
            peak_wet > peak_dry * 2.0,
            "master_gain=3.0 should noticeably raise peak: dry={peak_dry} wet={peak_wet}"
        );
    }

    #[test]
    fn master_gain_zero_silences_output() {
        let mut s = one_note_song();
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
        s.tracks[0].instrument = Instrument::new("unknown_synth"); // 未知 type は無音 (validate 側でエラー出力されるが render は黙ってスキップ)
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert_eq!(peak, 0.0);
    }

    #[test]
    fn saw_pad_renders_audio() {
        let mut s = one_note_song();
        s.tracks[0].instrument = Instrument::new("saw_pad");
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert!(
            peak > 0.1,
            "saw_pad should produce audible output, got {peak}"
        );
    }

    #[test]
    fn saw_pad_detune_cents_respected() {
        // detune_cents を変えても音は出る (params 取り出し→ closure→ render の経路が落ちないこと)
        let mut s = one_note_song();
        let mut inst = Instrument::new("saw_pad");
        inst.params
            .insert("detune_cents".into(), serde_json::json!(25.0));
        s.tracks[0].instrument = inst;
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert!(
            peak > 0.1,
            "saw_pad with custom detune should still produce audio, got {peak}"
        );
    }

    #[test]
    fn triangle_renders_audio() {
        let mut s = one_note_song();
        s.tracks[0].instrument = Instrument::new("triangle");
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert!(
            peak > 0.1,
            "triangle should produce audible output, got {peak}"
        );
    }

    #[test]
    fn square_renders_audio() {
        let mut s = one_note_song();
        s.tracks[0].instrument = Instrument::new("square");
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert!(
            peak > 0.1,
            "square should produce audible output, got {peak}"
        );
    }

    #[test]
    fn square_bass_alias_also_renders() {
        let mut s = one_note_song();
        s.tracks[0].instrument = Instrument::new("square_bass");
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert!(
            peak > 0.1,
            "square_bass should produce audible output, got {peak}"
        );
    }

    #[test]
    fn pulse_width_param_respected() {
        // pulse_width を変えても音は出る (実際の duty 差は周波数解析しないと見えないが、
        // params 取り出し→ closure→ render の経路が落ちないことを確認する)
        let mut s = one_note_song();
        let mut inst = Instrument::new("square");
        inst.params
            .insert("pulse_width".into(), serde_json::json!(0.2));
        s.tracks[0].instrument = inst;
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert!(
            peak > 0.1,
            "narrow pulse should still produce audio, got {peak}"
        );
    }

    #[test]
    fn saw_lead_renders_audio() {
        let mut s = one_note_song();
        s.tracks[0].instrument = Instrument::new("saw_lead");
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert!(
            peak > 0.1,
            "saw_lead should produce audible output, got {peak}"
        );
    }

    #[test]
    fn saw_alias_also_renders() {
        let mut s = one_note_song();
        s.tracks[0].instrument = Instrument::new("saw");
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert!(peak > 0.1, "saw should produce audible output, got {peak}");
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
        // saw (倍音豊富) に 500Hz lowpass を被せると RMS が下がるはず
        let mut s = one_note_song();
        s.tracks[0].instrument = Instrument::new("saw");
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
        // 倍音が削れるので RMS は明確に下がる (= 少なくとも 80%)
        assert!(
            rms_wet < rms_dry * 0.95,
            "lowpass should attenuate saw harmonics: dry={rms_dry} wet={rms_wet}"
        );
        // でも消えてはいない
        assert!(
            rms_wet > 0.01,
            "lowpass should not silence the track: {rms_wet}"
        );
    }

    #[test]
    fn distortion_fx_changes_waveform() {
        // distortion(amount=1) で出力ピークが圧縮される (元 saw のピーク値より低くなる)
        let mut s = one_note_song();
        s.tracks[0].instrument = Instrument::new("saw");
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
        let peak_dry = dry.iter().map(|(l, _)| l.abs()).fold(0.0_f32, f32::max);
        let peak_wet = wet.iter().map(|(l, _)| l.abs()).fold(0.0_f32, f32::max);
        // dry/wet の波形が変わっていれば OK (どちらが大きいかは tanh の make_up に依存するが、 サンプル単位で差があるはず)
        let diff_count = dry
            .iter()
            .zip(wet.iter())
            .filter(|(d, w)| (d.0 - w.0).abs() > 0.01)
            .count();
        assert!(
            diff_count > 100,
            "distortion should change samples: diff_count={diff_count} peak_dry={peak_dry} peak_wet={peak_wet}"
        );
    }

    #[test]
    fn delay_fx_extends_tail() {
        // 短いノートに delay (feedback>0、 mix>0) をかけると、 ノート末尾以降に音が残る
        let mut s = one_note_song();
        // note.dur=0.5 beat、 bpm=120 → 0.25 秒。 余韻 2 秒も含まれるが、 1 秒地点での RMS で確認
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
        // 1.5 秒以降のサンプル (delay echo が残る範囲) を確認
        let tail_start = (1.5 * SAMPLE_RATE as f32) as usize;
        if tail_start < buf.len() {
            let tail_rms: f32 = (buf[tail_start..].iter().map(|(l, _)| l * l).sum::<f32>()
                / (buf.len() - tail_start) as f32)
                .sqrt();
            assert!(
                tail_rms > 1e-4,
                "delay echo should leave audible tail: {tail_rms}"
            );
        }
    }

    #[test]
    fn fx_chain_applied_in_order() {
        // lowpass(500Hz、 倍音削減) → distortion(amount=1、 強 saturate) と
        // distortion → lowpass では saturate される波形が違うので、 結果は明確に違う。
        let mut s_dl = one_note_song();
        s_dl.tracks[0].instrument = Instrument::new("saw");
        s_dl.tracks[0].notes[0].dur = 2.0; // hold を長く取って fx の効果が乗る区間を確保
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
        s_ld.tracks[0].fx.swap(0, 1); // lowpass → distortion
        let buf_dl = render_to_buffer(&s_dl);
        let buf_ld = render_to_buffer(&s_ld);
        let diff: usize = buf_dl
            .iter()
            .zip(buf_ld.iter())
            .filter(|(a, b)| (a.0 - b.0).abs() > 0.001)
            .count();
        assert!(
            diff > 100,
            "fx order should matter (distortion→lowpass vs lowpass→distortion), diff={diff}"
        );
    }

    #[test]
    fn reverb_fx_extends_tail() {
        // 短いノートに reverb (mix=0.6) をかけると、 ノート末尾以降にも残響が残る
        let mut s = one_note_song();
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
        // note.dur=0.5 beat / bpm=120 = 0.25 秒。 余韻 2 秒のうち末尾 0.5 秒に reverb tail が乗っているはず
        let tail_start = buf.len().saturating_sub(SAMPLE_RATE as usize / 2);
        let tail_rms: f32 = (buf[tail_start..].iter().map(|(l, _)| l * l).sum::<f32>()
            / (buf.len() - tail_start) as f32)
            .sqrt();
        assert!(
            tail_rms > 1e-5,
            "reverb tail should leave audible energy: {tail_rms}"
        );
    }

    #[test]
    fn drum_kit_kick_renders_audio() {
        let mut s = one_note_song();
        s.tracks[0].instrument = Instrument::new("drum_kit");
        s.tracks[0].notes[0].pitch = Pitch::Name("kick".into());
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert!(
            peak > 0.1,
            "drum_kit kick should produce audible output, got {peak}"
        );
    }

    #[test]
    fn drum_kit_unknown_key_silently_skipped() {
        let mut s = one_note_song();
        s.tracks[0].instrument = Instrument::new("drum_kit");
        s.tracks[0].notes[0].pitch = Pitch::Name("zap".into()); // validate でエラー扱いだが render は黙ってスキップ
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert_eq!(peak, 0.0);
    }

    #[test]
    fn drum_kit_kit_param_changes_output() {
        // kit="909" と kit=未指定 (default) で kick の出力が変わる
        let mut s = one_note_song();
        s.tracks[0].instrument = Instrument::new("drum_kit");
        s.tracks[0].notes[0].pitch = Pitch::Name("kick".into());
        let buf_default = render_to_buffer(&s);

        let mut inst = Instrument::new("drum_kit");
        inst.params.insert("kit".into(), serde_json::json!("909"));
        s.tracks[0].instrument = inst;
        let buf_909 = render_to_buffer(&s);

        let diff: usize = buf_default
            .iter()
            .zip(buf_909.iter())
            .filter(|(a, b)| (a.0 - b.0).abs() > 0.001)
            .count();
        assert!(
            diff > 100,
            "kit param should change kick waveform: diff={diff}"
        );
    }

    #[test]
    fn soundfont_track_unknown_file_silently_skipped() {
        // file が存在しなくても render は落ちず無音で済む (validate 側で報告される責務)。
        let mut s = one_note_song();
        let mut inst = Instrument::new("soundfont");
        inst.params.insert(
            "file".into(),
            serde_json::json!("/nonexistent/codetta-test/missing.sf2"),
        );
        s.tracks[0].instrument = inst;
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert_eq!(peak, 0.0, "missing SF2 should leave silence (got {peak})");
    }

    #[test]
    fn soundfont_track_renders_when_sf2_available() {
        let Some(sf2) = std::env::var("CODETTA_TEST_SF2").ok() else {
            eprintln!("CODETTA_TEST_SF2 not set — skipping render-level SF2 integration test");
            return;
        };
        let mut s = one_note_song();
        s.tracks[0].notes[0].dur = 2.0; // ~1s @ 120bpm
        let mut inst = Instrument::new("soundfont");
        inst.params.insert("file".into(), serde_json::json!(sf2));
        inst.params.insert("preset".into(), serde_json::json!(0));
        s.tracks[0].instrument = inst;
        let buf = render_to_buffer(&s);
        let peak = buf
            .iter()
            .map(|(l, r)| l.abs().max(r.abs()))
            .fold(0.0_f32, f32::max);
        assert!(
            peak > 0.01,
            "SF2 render should produce audible output, got {peak}"
        );
    }

    #[test]
    fn soundfont_two_tracks_share_load_and_render_equivalently() {
        // 同一 SF2 を 2 つの track で参照したとき、 SF2 cache を経由しても
        // 「2 track の合算」 = 「1 track 単体 × 2 の合算」 になることを確認する。
        // cache 利用後の Synthesizer 状態が track 間で漏れていないことの担保。
        let Some(sf2) = std::env::var("CODETTA_TEST_SF2").ok() else {
            eprintln!("CODETTA_TEST_SF2 not set — skipping SF2 cache equivalence test");
            return;
        };
        let mut inst = Instrument::new("soundfont");
        inst.params.insert("file".into(), serde_json::json!(sf2));
        inst.params.insert("preset".into(), serde_json::json!(0));

        // baseline: 1 track のみ render
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

        // dual: 同一 SF2 / 同一 note を 2 track 並列 (片方 pan 中央 + volume 半分 ×2)
        let mut s_two = s_one.clone();
        s_two.tracks.push(s_two.tracks[0].clone());
        s_two.tracks[1].id = "t2".into();
        let buf_two = render_to_buffer(&s_two);

        // 出力長は等しい
        assert_eq!(buf_one.len(), buf_two.len(), "duration should match");

        // 2 track 合算は 1 track の概ね 2 倍 (soft clip による頭打ちで完全 2 倍にはならない)。
        // 期待値: 各サンプルが 2 倍された後 soft_clip(2x) を通った値。 つまり
        //   y2[i] = soft_clip(2 * soft_clip_inv(y1[i]))
        // を直接検証するのは過剰なので、 単純に「ピークが上がる」「波形がほぼ符号一致」のみ確認。
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

        // サンプルごとの符号 (rough waveform 一致) を確認: 全サンプル中、
        // L チャンネルで両方とも sign が一致する割合が高いはず。
        let mut total = 0usize;
        let mut agree = 0usize;
        for ((l1, _), (l2, _)) in buf_one.iter().zip(buf_two.iter()) {
            if l1.abs() < 1e-4 && l2.abs() < 1e-4 {
                continue; // 無音区間 (両方 0) は除外
            }
            total += 1;
            if l1.signum() == l2.signum() {
                agree += 1;
            }
        }
        // 同じ SF2 / 同じ note なので波形は単なる加算 + soft_clip。 符号一致率は十分高いはず。
        assert!(
            total > 0 && (agree as f32 / total as f32) > 0.95,
            "two-track waveform sign should match single-track baseline: {agree}/{total}"
        );
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
