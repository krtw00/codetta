//! 自前ループ実装の 1 voice (オシレータ + ADSR)。
//!
//! `fundsp` を使わずに済むかを判断するための「自前版」プロトタイプ。
//! 現状は `sin` / `saw` / `square` (PolyBLEP) / `triangle` (解析式) / `saw_pad` (saw × 3 detune) を実装済み。
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

/// 1 ノート分の PolyBLEP square / pulse + ADSR を生成し、 mono バッファとして返す。
///
/// `pulse_width` (0.05-0.95 にクランプ) で duty を制御。 0.5 で対称矩形、 それ以外で PWM。
/// 立ち上がり (phase=0、 -1→+1) と立ち下がり (phase=pulse_width、 +1→-1) の 2 箇所に
/// PolyBLEP 補正を適用する。
pub fn render_voice_square(
    freq_hz: f32,
    hold_sec: f32,
    adsr: AdsrParams,
    pulse_width: f32,
) -> Vec<f32> {
    let sr = SAMPLE_RATE as f32;
    let dt = freq_hz / sr;
    let pw = pulse_width.clamp(0.05, 0.95);
    let env = build_envelope(hold_sec, adsr);
    let mut phase = 0.0_f32; // 0..1
    let mut out = Vec::with_capacity(env.len());

    for &e in &env {
        let naive = if phase < pw { 1.0 } else { -1.0 };
        // 立ち上がり @ phase=0 (上向きステップ) → polyblep を加算
        // 立ち下がり @ phase=pw (下向きステップ) → 位相を pw だけずらして polyblep を減算
        let phase_at_fall = if phase >= pw { phase - pw } else { phase + 1.0 - pw };
        let y = naive + polyblep(phase, dt) - polyblep(phase_at_fall, dt);
        out.push(y * e);
        phase += dt;
        if phase >= 1.0 {
            phase -= 1.0;
        }
    }
    out
}

/// 1 ノート分の triangle + ADSR を生成し、 mono バッファとして返す。
///
/// 解析式 `1 - 4 * |phase - 0.5|` で -1..+1 振幅の三角波を直接生成する。
/// ステップ不連続がない (傾きの不連続のみ) ので 1/f² で減衰し、 PolyBLEP 補正なしでも
/// エイリアシングは穏やか。 高音域で目立つようになれば BLAMP に差し替える。
pub fn render_voice_triangle(freq_hz: f32, hold_sec: f32, adsr: AdsrParams) -> Vec<f32> {
    let sr = SAMPLE_RATE as f32;
    let dt = freq_hz / sr;
    let env = build_envelope(hold_sec, adsr);
    let mut phase = 0.0_f32; // 0..1
    let mut out = Vec::with_capacity(env.len());

    for &e in &env {
        let y = 1.0 - 4.0 * (phase - 0.5).abs();
        out.push(y * e);
        phase += dt;
        if phase >= 1.0 {
            phase -= 1.0;
        }
    }
    out
}

/// 1 ノート分の saw × 3 detune (saw_pad) + ADSR を生成し、 mono バッファとして返す。
///
/// 中央 voice (freq そのまま) + `+detune_cents` voice + `-detune_cents` voice を 1/3 ずつ合成。
/// 各 voice は `render_voice_saw` と同じ PolyBLEP 補正を適用。 セントから比へは `2^(cents/1200)`。
/// `detune_cents` は 0-50 にクランプ (sound.md の範囲)。 0 のときは事実上 saw 単音 (3 つ揃って同位相に
/// 走るので干渉なし)。 ハーモニカルな厚みを得るのが目的なので、 デフォルト 10 セントで十分なうねりが出る。
///
/// alloc 削減のため `render_voice_saw` を 3 回呼ぶのではなく 1 ループ内で 3 位相を進める。
pub fn render_voice_saw_pad(
    freq_hz: f32,
    hold_sec: f32,
    adsr: AdsrParams,
    detune_cents: f32,
) -> Vec<f32> {
    let sr = SAMPLE_RATE as f32;
    let cents = detune_cents.clamp(0.0, 50.0);
    let ratio = 2.0_f32.powf(cents / 1200.0);
    let freqs = [freq_hz / ratio, freq_hz, freq_hz * ratio];
    let dts = [freqs[0] / sr, freqs[1] / sr, freqs[2] / sr];
    let env = build_envelope(hold_sec, adsr);
    let mut phases = [0.0_f32; 3];
    let mut out = Vec::with_capacity(env.len());

    for &e in &env {
        let mut y = 0.0_f32;
        for k in 0..3 {
            let naive = 2.0 * phases[k] - 1.0;
            y += naive - polyblep(phases[k], dts[k]);
            phases[k] += dts[k];
            if phases[k] >= 1.0 {
                phases[k] -= 1.0;
            }
        }
        out.push((y / 3.0) * e);
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

    #[test]
    fn square_voice_length_matches_hold_plus_release() {
        let adsr = AdsrParams {
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.1,
        };
        let v = render_voice_square(440.0, 0.5, adsr, 0.5);
        let expected = (0.5 * SAMPLE_RATE as f32) as usize + (0.1 * SAMPLE_RATE as f32) as usize;
        assert_eq!(v.len(), expected);
    }

    #[test]
    fn square_voice_ends_near_zero() {
        let v = render_voice_square(440.0, 0.05, AdsrParams::default(), 0.5);
        let last = *v.last().unwrap();
        assert!(last.abs() < 0.05, "release tail should be near zero, got {last}");
    }

    #[test]
    fn square_voice_amplitude_within_unit() {
        let v = render_voice_square(440.0, 0.1, AdsrParams::default(), 0.5);
        let max = v.iter().cloned().fold(0.0_f32, f32::max);
        let min = v.iter().cloned().fold(0.0_f32, f32::min);
        assert!(max <= 1.0 && min >= -1.0, "out of range: [{min}, {max}]");
        assert!(max > 0.4 && min < -0.4, "square should swing both rails: [{min}, {max}]");
    }

    #[test]
    fn square_voice_traverses_range() {
        let adsr = AdsrParams {
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
        };
        let v = render_voice_square(220.0, 0.05, adsr, 0.5);
        let span = v.iter().cloned().fold(0.0_f32, f32::max)
            - v.iter().cloned().fold(0.0_f32, f32::min);
        assert!(span > 1.5, "square peak-to-peak too small: {span}");
    }

    #[test]
    fn triangle_voice_length_matches_hold_plus_release() {
        let adsr = AdsrParams {
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.1,
        };
        let v = render_voice_triangle(440.0, 0.5, adsr);
        let expected = (0.5 * SAMPLE_RATE as f32) as usize + (0.1 * SAMPLE_RATE as f32) as usize;
        assert_eq!(v.len(), expected);
    }

    #[test]
    fn triangle_voice_ends_near_zero() {
        let v = render_voice_triangle(440.0, 0.05, AdsrParams::default());
        let last = *v.last().unwrap();
        assert!(last.abs() < 0.05, "release tail should be near zero, got {last}");
    }

    #[test]
    fn triangle_voice_amplitude_within_unit() {
        let v = render_voice_triangle(440.0, 0.1, AdsrParams::default());
        let max = v.iter().cloned().fold(0.0_f32, f32::max);
        let min = v.iter().cloned().fold(0.0_f32, f32::min);
        assert!(max <= 1.0 && min >= -1.0, "out of range: [{min}, {max}]");
        assert!(max > 0.4 && min < -0.4, "triangle should swing both rails: [{min}, {max}]");
    }

    #[test]
    fn triangle_voice_traverses_range() {
        let adsr = AdsrParams {
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
        };
        let v = render_voice_triangle(220.0, 0.05, adsr);
        let span = v.iter().cloned().fold(0.0_f32, f32::max)
            - v.iter().cloned().fold(0.0_f32, f32::min);
        // 解析式そのままなのでほぼ ±1 まで届く
        assert!(span > 1.8, "triangle peak-to-peak too small: {span}");
    }

    #[test]
    fn saw_pad_voice_length_matches_hold_plus_release() {
        let adsr = AdsrParams {
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.1,
        };
        let v = render_voice_saw_pad(440.0, 0.5, adsr, 10.0);
        let expected = (0.5 * SAMPLE_RATE as f32) as usize + (0.1 * SAMPLE_RATE as f32) as usize;
        assert_eq!(v.len(), expected);
    }

    #[test]
    fn saw_pad_voice_ends_near_zero() {
        let v = render_voice_saw_pad(440.0, 0.05, AdsrParams::default(), 10.0);
        let last = *v.last().unwrap();
        assert!(last.abs() < 0.05, "release tail should be near zero, got {last}");
    }

    #[test]
    fn saw_pad_voice_amplitude_within_unit() {
        let v = render_voice_saw_pad(440.0, 0.1, AdsrParams::default(), 10.0);
        let max = v.iter().cloned().fold(0.0_f32, f32::max);
        let min = v.iter().cloned().fold(0.0_f32, f32::min);
        // 3 voice を 1/3 ずつ合成しているので最悪でも ±1 に収まる
        assert!(max <= 1.0 && min >= -1.0, "out of range: [{min}, {max}]");
        // detune してもメインの saw が振り切るタイミングがあるので両極に届く
        assert!(max > 0.4 && min < -0.4, "saw_pad should swing wide: [{min}, {max}]");
    }

    #[test]
    fn saw_pad_detune_cents_clamped() {
        // 100 セント要求 → 50 にクランプされてもクラッシュせず音が出る
        let adsr = AdsrParams {
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
        };
        let v = render_voice_saw_pad(220.0, 0.05, adsr, 100.0);
        let span = v.iter().cloned().fold(0.0_f32, f32::max)
            - v.iter().cloned().fold(0.0_f32, f32::min);
        assert!(span > 1.0, "saw_pad with extreme detune should still oscillate: {span}");
    }

    #[test]
    fn saw_pad_zero_detune_matches_single_saw_shape() {
        // detune_cents = 0 のとき 3 voice が同位相に走るので、 出力は単音 saw とほぼ同じ波形
        // (1/3 + 1/3 + 1/3 = 1.0)。 サンプル単位の許容誤差は PolyBLEP の浮動小数演算順序差のみ。
        let adsr = AdsrParams {
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
        };
        let pad = render_voice_saw_pad(220.0, 0.05, adsr, 0.0);
        let saw = render_voice_saw(220.0, 0.05, adsr);
        assert_eq!(pad.len(), saw.len());
        for (p, s) in pad.iter().zip(saw.iter()) {
            assert!((p - s).abs() < 1e-4, "expected equivalence at zero detune: {p} vs {s}");
        }
    }

    #[test]
    fn square_pulse_width_clamped() {
        // 0.0 や 1.0 が来てもクラッシュせず音が出る (内部で 0.05-0.95 にクランプ)
        let adsr = AdsrParams {
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.0,
        };
        let narrow = render_voice_square(440.0, 0.05, adsr, 0.0);
        let wide = render_voice_square(440.0, 0.05, adsr, 1.0);
        let span_narrow = narrow.iter().cloned().fold(0.0_f32, f32::max)
            - narrow.iter().cloned().fold(0.0_f32, f32::min);
        let span_wide = wide.iter().cloned().fold(0.0_f32, f32::max)
            - wide.iter().cloned().fold(0.0_f32, f32::min);
        // 極端な pulse_width でも両極に振れていれば clamp が効いている
        assert!(span_narrow > 1.0, "narrow pulse too small: {span_narrow}");
        assert!(span_wide > 1.0, "wide pulse too small: {span_wide}");
    }
}
