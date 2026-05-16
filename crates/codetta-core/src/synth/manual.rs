//! 自前ループ実装の 1 voice (sin オシレータ + ADSR)。
//!
//! `fundsp` を使わずに済むかを判断するための「自前版」プロトタイプ。
//! Phase 0 first cut では `sin` のみ。 saw / square / triangle は判断後に追加する。
//!
//! Envelope 曲線は線形セグメント。 sound.md は「指数」を最終形と書いているが、
//! Phase 0 first cut は線形で十分音になる (差し替えは Phase 1+)。

use std::f32::consts::TAU;

use super::{AdsrParams, SAMPLE_RATE};

/// 1 ノート分の sin + ADSR を生成し、 mono バッファとして返す。
///
/// 返るバッファ長は `hold_sec + release` 相当 (= attack/decay/sustain で `hold_sec`
/// 持続し、 そのあと release)。 attack + decay が `hold_sec` を超える短い音でも
/// 最低 1 サンプルは返す。
pub fn render_voice(freq_hz: f32, hold_sec: f32, adsr: AdsrParams) -> Vec<f32> {
    let sr = SAMPLE_RATE as f32;
    let hold_samples = ((hold_sec * sr).max(1.0)) as usize;
    let release_samples = ((adsr.release * sr).max(1.0)) as usize;
    let total = hold_samples + release_samples;

    let attack_samples = ((adsr.attack * sr).max(0.0)) as usize;
    let decay_samples = ((adsr.decay * sr).max(1.0)) as usize;

    let phase_inc = TAU * freq_hz / sr;
    let mut phase = 0.0_f32;
    let mut out = Vec::with_capacity(total);

    for i in 0..total {
        let env = if i < hold_samples {
            // attack → decay → sustain
            if i < attack_samples {
                // 0 → 1
                i as f32 / attack_samples.max(1) as f32
            } else if i < attack_samples + decay_samples {
                // 1 → sustain
                let t = (i - attack_samples) as f32 / decay_samples as f32;
                1.0 + (adsr.sustain - 1.0) * t
            } else {
                adsr.sustain
            }
        } else {
            // release: sustain → 0
            let t = (i - hold_samples) as f32 / release_samples as f32;
            adsr.sustain * (1.0 - t)
        };

        out.push(phase.sin() * env);
        phase += phase_inc;
        if phase > TAU {
            phase -= TAU;
        }
    }
    out
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
}
