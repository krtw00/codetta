//! 自前ループ実装の 1 voice (オシレータ + ADSR)。
//!
//! `fundsp` を使わずに済むかを判断するための「自前版」プロトタイプ。
//! 現状は `sin` と `saw` (PolyBLEP) を実装済み。 残り square / triangle / saw_pad は順次追加。
//!
//! Envelope 曲線は線形セグメント。 sound.md は「指数」を最終形と書いているが、
//! Phase 0 first cut は線形で十分音になる (差し替えは Phase 1+)。

use std::f32::consts::TAU;

use super::{AdsrParams, SAMPLE_RATE};

/// 1 ノート分の ADSR 包絡 (hold + release) を生成する。
///
/// 長さは `hold_sec + release` 相当 (= attack/decay/sustain で `hold_sec` 持続し、
/// そのあと release)。 attack + decay が `hold_sec` を超える短い音でも最低 1 サンプルは返す。
fn build_envelope(hold_sec: f32, adsr: AdsrParams) -> Vec<f32> {
    let sr = SAMPLE_RATE as f32;
    let hold_samples = ((hold_sec * sr).max(1.0)) as usize;
    let release_samples = ((adsr.release * sr).max(1.0)) as usize;
    let total = hold_samples + release_samples;

    let attack_samples = ((adsr.attack * sr).max(0.0)) as usize;
    let decay_samples = ((adsr.decay * sr).max(1.0)) as usize;

    let mut env = Vec::with_capacity(total);
    for i in 0..total {
        let v = if i < hold_samples {
            if i < attack_samples {
                i as f32 / attack_samples.max(1) as f32
            } else if i < attack_samples + decay_samples {
                let t = (i - attack_samples) as f32 / decay_samples as f32;
                1.0 + (adsr.sustain - 1.0) * t
            } else {
                adsr.sustain
            }
        } else {
            let t = (i - hold_samples) as f32 / release_samples as f32;
            adsr.sustain * (1.0 - t)
        };
        env.push(v);
    }
    env
}

/// 1 ノート分の sin + ADSR を生成し、 mono バッファとして返す。
pub fn render_voice(freq_hz: f32, hold_sec: f32, adsr: AdsrParams) -> Vec<f32> {
    let sr = SAMPLE_RATE as f32;
    let env = build_envelope(hold_sec, adsr);
    let phase_inc = TAU * freq_hz / sr;
    let mut phase = 0.0_f32;
    let mut out = Vec::with_capacity(env.len());

    for &e in &env {
        out.push(phase.sin() * e);
        phase += phase_inc;
        if phase > TAU {
            phase -= TAU;
        }
    }
    out
}

/// 1 ノート分の PolyBLEP saw + ADSR を生成し、 mono バッファとして返す。
///
/// naive saw (`2*phase - 1`) のステップ不連続は dt 幅でエイリアシングを生むため、
/// 位相リセットの前後で polynomial 補正を適用する (Välimäki & Huovilainen 2007 系)。
pub fn render_voice_saw(freq_hz: f32, hold_sec: f32, adsr: AdsrParams) -> Vec<f32> {
    let sr = SAMPLE_RATE as f32;
    let dt = freq_hz / sr; // 1 サンプルあたりの位相増分 (0..1)
    let env = build_envelope(hold_sec, adsr);
    let mut phase = 0.0_f32; // 0..1
    let mut out = Vec::with_capacity(env.len());

    for &e in &env {
        let naive = 2.0 * phase - 1.0;
        let y = naive - polyblep(phase, dt);
        out.push(y * e);
        phase += dt;
        if phase >= 1.0 {
            phase -= 1.0;
        }
    }
    out
}

/// PolyBLEP 補正値。 位相が 0 直後 (`t < dt`) と 1 直前 (`t > 1 - dt`) に局所的な
/// 多項式を加減算してステップ不連続を緩和する。 dt は 1 サンプル分の位相増分。
fn polyblep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let t = t / dt;
        2.0 * t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + 2.0 * t + 1.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_length_matches_hold_plus_release() {
        let adsr = AdsrParams {
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.1,
        };
        let v = render_voice(440.0, 0.5, adsr);
        let expected = (0.5 * SAMPLE_RATE as f32) as usize + (0.1 * SAMPLE_RATE as f32) as usize;
        assert_eq!(v.len(), expected);
    }

    #[test]
    fn voice_ends_near_zero() {
        let v = render_voice(440.0, 0.05, AdsrParams::default());
        let last = *v.last().unwrap();
        assert!(last.abs() < 0.02, "release tail should be near zero, got {last}");
    }

    #[test]
    fn voice_amplitude_within_unit() {
        let v = render_voice(440.0, 0.1, AdsrParams::default());
        let max = v.iter().cloned().fold(0.0_f32, f32::max);
        let min = v.iter().cloned().fold(0.0_f32, f32::min);
        assert!(max <= 1.0 && min >= -1.0, "out of range: [{min}, {max}]");
        // attack/decay/sustain で最低限のエネルギーが出ている
        assert!(max > 0.5);
    }

    #[test]
    fn saw_voice_length_matches_hold_plus_release() {
        let adsr = AdsrParams {
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.1,
        };
        let v = render_voice_saw(440.0, 0.5, adsr);
        let expected = (0.5 * SAMPLE_RATE as f32) as usize + (0.1 * SAMPLE_RATE as f32) as usize;
        assert_eq!(v.len(), expected);
    }

    #[test]
    fn saw_voice_ends_near_zero() {
        let v = render_voice_saw(440.0, 0.05, AdsrParams::default());
        let last = *v.last().unwrap();
        assert!(last.abs() < 0.05, "release tail should be near zero, got {last}");
    }

    #[test]
    fn saw_voice_amplitude_within_unit() {
        let v = render_voice_saw(440.0, 0.1, AdsrParams::default());
        let max = v.iter().cloned().fold(0.0_f32, f32::max);
        let min = v.iter().cloned().fold(0.0_f32, f32::min);
        // PolyBLEP 補正後でも振幅は ±1 内 (envelope 係数 ≤ 1 を考慮)
        assert!(max <= 1.0 && min >= -1.0, "out of range: [{min}, {max}]");
        // saw は両極に振れる
        assert!(max > 0.4 && min < -0.4, "saw should swing wide: [{min}, {max}]");
    }

    #[test]
    fn saw_voice_traverses_range() {
        // naive saw は周期内で -1 → +1 → -1 と推移する。 PolyBLEP 補正で角は丸まるが
        // hold 中盤を見れば概ね sustain * 1.0 近辺まで到達するはず。
        let adsr = AdsrParams {
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
        };
        let v = render_voice_saw(220.0, 0.05, adsr);
        let span = v.iter().cloned().fold(0.0_f32, f32::max)
            - v.iter().cloned().fold(0.0_f32, f32::min);
        assert!(span > 1.5, "saw peak-to-peak too small: {span}");
    }
}
