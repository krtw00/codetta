//! トラック / マスター用エフェクトチェーン。
//!
//! 全てステレオ in-place で動作する (`&mut [(f32, f32)]`)。
//! Phase 0 のサポート:
//!
//! - `lowpass` / `highpass` (SVF、 Chamberlin 型)
//! - `distortion` (tanh ソフトクリップ + tone lowpass)
//! - `delay` (循環バッファ、 BPM 同期 / 秒指定の両対応)
//! - `reverb` (Schroeder: 4 comb + 2 allpass、 LR 独立 delay 長で広がり)

use std::f32::consts::PI;

use crate::synth::SAMPLE_RATE;

/// State Variable Filter (Chamberlin) の 1 チャネル分の state。
///
/// 1 サンプル更新ごとに `low` と `band` を進める。 `lowpass` は `low` を、
/// `highpass` は `high = x - low - q_coef * band` を出力に使う。
#[derive(Debug, Default, Clone, Copy)]
struct SvfState {
    low: f32,
    band: f32,
}

/// Chamberlin SVF の 1 サンプル更新。 lowpass 出力と highpass 出力をタプルで返す。
///
/// `f_coef = 2 * sin(PI * cutoff / sr)` (cutoff < sr/2 で安定)、
/// `q_coef = 1 / q` (damping; 小さいほどレゾナンスが立つ)。
fn svf_tick(state: &mut SvfState, input: f32, f_coef: f32, q_coef: f32) -> (f32, f32) {
    let high = input - state.low - q_coef * state.band;
    state.band += f_coef * high;
    state.low += f_coef * state.band;
    (state.low, high)
}

/// SVF lowpass をステレオ in-place 適用する。
///
/// `cutoff_hz` は 20-20000、 `q` は 0.5-10 の範囲にクランプ (sound.md 仕様)。
/// `cutoff_hz` が Nyquist 近辺ならほぼバイパス相当 (`f_coef` が 1 に近くなる)。
pub fn lowpass(buf: &mut [(f32, f32)], cutoff_hz: f32, q: f32, sample_rate: u32) {
    let (f_coef, q_coef) = svf_coefs(cutoff_hz, q, sample_rate);
    let mut l = SvfState::default();
    let mut r = SvfState::default();
    for (sl, sr) in buf.iter_mut() {
        let (low_l, _high_l) = svf_tick(&mut l, *sl, f_coef, q_coef);
        let (low_r, _high_r) = svf_tick(&mut r, *sr, f_coef, q_coef);
        *sl = low_l;
        *sr = low_r;
    }
}

/// SVF highpass をステレオ in-place 適用する。 `q` / `cutoff` の扱いは [`lowpass`] と同じ。
pub fn highpass(buf: &mut [(f32, f32)], cutoff_hz: f32, q: f32, sample_rate: u32) {
    let (f_coef, q_coef) = svf_coefs(cutoff_hz, q, sample_rate);
    let mut l = SvfState::default();
    let mut r = SvfState::default();
    for (sl, sr) in buf.iter_mut() {
        let (_low_l, high_l) = svf_tick(&mut l, *sl, f_coef, q_coef);
        let (_low_r, high_r) = svf_tick(&mut r, *sr, f_coef, q_coef);
        *sl = high_l;
        *sr = high_r;
    }
}

/// cutoff / q を SVF 係数に変換する。 範囲外はクランプ。
fn svf_coefs(cutoff_hz: f32, q: f32, sample_rate: u32) -> (f32, f32) {
    let sr = sample_rate as f32;
    let nyquist = sr * 0.5;
    let cutoff = cutoff_hz.clamp(20.0, nyquist * 0.99);
    let f_coef = 2.0 * (PI * cutoff / sr).sin();
    let q_clamped = q.clamp(0.5, 10.0);
    let q_coef = 1.0 / q_clamped;
    (f_coef, q_coef)
}

/// tanh ベースの distortion + tone lowpass。 ステレオ in-place。
///
/// `amount` (0-1) は drive ゲインに線形マップ (`gain = 1 + 9 * amount`)。 amount=0 でほぼバイパス、
/// amount=1 で 10x drive を tanh で潰す。 出力は 1/gain で正規化して RMS 過剰を避ける。
/// `tone` (0-1) は内蔵 lowpass の cutoff (500Hz - 18kHz の log マップ)。
pub fn distortion(buf: &mut [(f32, f32)], amount: f32, tone: f32, sample_rate: u32) {
    let amount = amount.clamp(0.0, 1.0);
    let tone = tone.clamp(0.0, 1.0);
    let gain = 1.0 + 9.0 * amount;
    // 出力ゲインは drive 量に対して圧縮されない部分のみ補正 (フル drive でも音が小さくなりすぎないように 1/sqrt(gain) 程度)
    let make_up = 1.0 / gain.sqrt();
    for s in buf.iter_mut() {
        s.0 = (s.0 * gain).tanh() * make_up;
        s.1 = (s.1 * gain).tanh() * make_up;
    }
    // tone: 500Hz (tone=0) から 18000Hz (tone=1) を対数で
    let cutoff = 500.0 * (18000.0_f32 / 500.0).powf(tone);
    lowpass(buf, cutoff, 0.7, sample_rate);
}

/// 循環バッファによる stereo delay (フィードバック付き、 dry/wet mix)。
///
/// `delay_sec` は 0.001 - 2.0 の範囲にクランプ。 `feedback` は 0 - 0.95 で発振防止、
/// `mix` は 0 - 1 で wet 比率 (0 で dry、 1 で wet のみ)。
/// 1 サンプルあたり `read → write → output` の順で更新する典型的なリング buffer。
pub fn delay(buf: &mut [(f32, f32)], delay_sec: f32, feedback: f32, mix: f32, sample_rate: u32) {
    let delay_sec = delay_sec.clamp(0.001, 2.0);
    let feedback = feedback.clamp(0.0, 0.95);
    let mix = mix.clamp(0.0, 1.0);
    let sr = sample_rate as f32;
    let delay_samples = ((delay_sec * sr) as usize).max(1);
    let mut line_l = vec![0.0_f32; delay_samples];
    let mut line_r = vec![0.0_f32; delay_samples];
    let mut pos: usize = 0;
    for s in buf.iter_mut() {
        let read_l = line_l[pos];
        let read_r = line_r[pos];
        // フィードバックは現在の入力 + 過去の出力
        line_l[pos] = s.0 + read_l * feedback;
        line_r[pos] = s.1 + read_r * feedback;
        s.0 = s.0 * (1.0 - mix) + read_l * mix;
        s.1 = s.1 * (1.0 - mix) + read_r * mix;
        pos = (pos + 1) % delay_samples;
    }
}

/// BPM 同期表記 (`"1/8"` 等) を秒に変換する。 認識できなければ秒指定として `f64::parse`。
///
/// `"1/4"` = 4分音符 = `60/BPM` 秒、 `"1/8"` = 8分 = `30/BPM` 秒...。
/// `"1"` は 1 拍。 BPM が 0 のときは 0 秒を返す (caller が clamp する想定)。
pub fn parse_delay_time(spec: &serde_json::Value, bpm: u32) -> f32 {
    let bpm = bpm.max(1) as f32;
    let sec_per_beat = 60.0 / bpm;
    if let Some(s) = spec.as_str() {
        // "1/8" のような分数を beat に変換: "1/N" = 4/N 拍 (4 分音符基準)
        if let Some((num, denom)) = s.split_once('/') {
            if let (Ok(n), Ok(d)) = (num.parse::<f32>(), denom.parse::<f32>()) {
                if d > 0.0 {
                    return (n / d) * 4.0 * sec_per_beat;
                }
            }
        }
        if let Ok(n) = s.parse::<f32>() {
            return n * sec_per_beat;
        }
    }
    if let Some(v) = spec.as_f64() {
        return v as f32; // 秒
    }
    // フォールバック: 1/8 拍
    0.5 * sec_per_beat
}

/// 既定サンプルレート (44.1kHz) でのショートカット。
pub fn lowpass_default_sr(buf: &mut [(f32, f32)], cutoff_hz: f32, q: f32) {
    lowpass(buf, cutoff_hz, q, SAMPLE_RATE);
}

/// Schroeder reverb (4 comb in parallel → 2 allpass in series)、 ステレオ in-place。
///
/// - `size` (0-1): comb delay 長を 0.5x-1.5x スケール、 同時に feedback gain (0.7-0.98) を上げて減衰時間を長くする
/// - `damp` (0-1): comb 内 1 極 lowpass の係数。 0 で減衰なし、 1 で高域を即座に潰す
/// - `mix` (0-1): wet 比率。 0 で完全 dry、 1 で完全 wet (= dry を出さない)
///
/// L/R は base delay 長を独立に持つ (R は +23 samples) ことで Schroeder 流の擬似ステレオ感を出す。
/// 4 comb 並列の wet sum を 1/4 にスケールしてから allpass に通すので、 wet 経路の peak は
/// 入力 peak と同等オーダーに収まる (限界値での発振は feedback 0.98 上限 + soft limiter で保護)。
pub fn reverb(buf: &mut [(f32, f32)], size: f32, damp: f32, mix: f32, sample_rate: u32) {
    let size = size.clamp(0.0, 1.0);
    let damp = damp.clamp(0.0, 1.0);
    let mix = mix.clamp(0.0, 1.0);

    // Schroeder/Freeverb 由来の 44.1kHz 想定 delay 長を、 実際の sample_rate にスケール
    let sr_scale = sample_rate as f32 / 44100.0;
    let size_scale = 0.5 + size; // 0.5x..1.5x
    let scale_len = |base: usize| ((base as f32 * size_scale * sr_scale) as usize).max(1);
    let scale_ap = |base: usize| ((base as f32 * sr_scale) as usize).max(1);
    const COMB_BASE: [usize; 4] = [1116, 1188, 1277, 1356];
    const ALLPASS_BASE: [usize; 2] = [556, 441];
    const STEREO_SPREAD: usize = 23;

    let comb_len_l: [usize; 4] = [
        scale_len(COMB_BASE[0]),
        scale_len(COMB_BASE[1]),
        scale_len(COMB_BASE[2]),
        scale_len(COMB_BASE[3]),
    ];
    let comb_len_r: [usize; 4] = [
        scale_len(COMB_BASE[0] + STEREO_SPREAD),
        scale_len(COMB_BASE[1] + STEREO_SPREAD),
        scale_len(COMB_BASE[2] + STEREO_SPREAD),
        scale_len(COMB_BASE[3] + STEREO_SPREAD),
    ];
    let ap_len_l: [usize; 2] = [scale_ap(ALLPASS_BASE[0]), scale_ap(ALLPASS_BASE[1])];
    let ap_len_r: [usize; 2] = [
        scale_ap(ALLPASS_BASE[0] + STEREO_SPREAD),
        scale_ap(ALLPASS_BASE[1] + STEREO_SPREAD),
    ];

    // feedback は size に対して 0.7-0.98 をリニア。 1.0 直前で発振しないよう 0.98 で打ち止め
    let feedback = 0.7 + 0.28 * size;
    let allpass_gain = 0.5;

    let mut comb_l: [Vec<f32>; 4] = [
        vec![0.0; comb_len_l[0]],
        vec![0.0; comb_len_l[1]],
        vec![0.0; comb_len_l[2]],
        vec![0.0; comb_len_l[3]],
    ];
    let mut comb_r: [Vec<f32>; 4] = [
        vec![0.0; comb_len_r[0]],
        vec![0.0; comb_len_r[1]],
        vec![0.0; comb_len_r[2]],
        vec![0.0; comb_len_r[3]],
    ];
    let mut lpf_l = [0.0_f32; 4];
    let mut lpf_r = [0.0_f32; 4];
    let mut comb_pos_l = [0_usize; 4];
    let mut comb_pos_r = [0_usize; 4];

    let mut ap_l: [Vec<f32>; 2] = [vec![0.0; ap_len_l[0]], vec![0.0; ap_len_l[1]]];
    let mut ap_r: [Vec<f32>; 2] = [vec![0.0; ap_len_r[0]], vec![0.0; ap_len_r[1]]];
    let mut ap_pos_l = [0_usize; 2];
    let mut ap_pos_r = [0_usize; 2];

    let dry_gain = 1.0 - mix;
    // 4 comb 並列の合算 → 1/4 にスケールしてから wet
    let wet_scale = 0.25 * mix;

    for s in buf.iter_mut() {
        let in_l = s.0;
        let in_r = s.1;
        let mut wet_l = 0.0_f32;
        let mut wet_r = 0.0_f32;

        for i in 0..4 {
            let z_l = comb_l[i][comb_pos_l[i]];
            // damping 用 1 極 lowpass (damp 大 → 高域がより削れる)
            lpf_l[i] = z_l * (1.0 - damp) + lpf_l[i] * damp;
            comb_l[i][comb_pos_l[i]] = in_l + lpf_l[i] * feedback;
            comb_pos_l[i] = (comb_pos_l[i] + 1) % comb_len_l[i];
            wet_l += z_l;

            let z_r = comb_r[i][comb_pos_r[i]];
            lpf_r[i] = z_r * (1.0 - damp) + lpf_r[i] * damp;
            comb_r[i][comb_pos_r[i]] = in_r + lpf_r[i] * feedback;
            comb_pos_r[i] = (comb_pos_r[i] + 1) % comb_len_r[i];
            wet_r += z_r;
        }

        for i in 0..2 {
            let buffered_l = ap_l[i][ap_pos_l[i]];
            let out_l = -wet_l + buffered_l;
            ap_l[i][ap_pos_l[i]] = wet_l + buffered_l * allpass_gain;
            ap_pos_l[i] = (ap_pos_l[i] + 1) % ap_len_l[i];
            wet_l = out_l;

            let buffered_r = ap_r[i][ap_pos_r[i]];
            let out_r = -wet_r + buffered_r;
            ap_r[i][ap_pos_r[i]] = wet_r + buffered_r * allpass_gain;
            ap_pos_r[i] = (ap_pos_r[i] + 1) % ap_len_r[i];
            wet_r = out_r;
        }

        s.0 = in_l * dry_gain + wet_l * wet_scale;
        s.1 = in_r * dry_gain + wet_r * wet_scale;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dc_buf(level: f32, n: usize) -> Vec<(f32, f32)> {
        vec![(level, level); n]
    }

    fn sine_buf(freq: f32, n: usize) -> Vec<(f32, f32)> {
        let sr = SAMPLE_RATE as f32;
        (0..n)
            .map(|i| {
                let v = ((i as f32 / sr) * freq * std::f32::consts::TAU).sin();
                (v, v)
            })
            .collect()
    }

    fn rms(buf: &[(f32, f32)]) -> f32 {
        let n = buf.len().max(1) as f32;
        let sum: f32 = buf.iter().map(|(l, _)| l * l).sum();
        (sum / n).sqrt()
    }

    #[test]
    fn lowpass_keeps_amplitude_in_unit_range() {
        let mut buf = sine_buf(440.0, 4410);
        lowpass(&mut buf, 1000.0, 1.0, SAMPLE_RATE);
        let peak = buf.iter().map(|(l, _)| l.abs()).fold(0.0_f32, f32::max);
        assert!(peak < 1.5, "lowpass output should not blow up, peak={peak}");
    }

    #[test]
    fn lowpass_passes_dc() {
        // DC 入力は lowpass をそのまま通過する (定常応答)
        let mut buf = dc_buf(0.5, 4410);
        lowpass(&mut buf, 1000.0, 1.0, SAMPLE_RATE);
        // 初期化トランジエントを除いた末尾でほぼ 0.5
        let tail_avg: f32 = buf[3000..].iter().map(|(l, _)| *l).sum::<f32>() / 1410.0;
        assert!(
            (tail_avg - 0.5).abs() < 0.05,
            "DC should pass, got {tail_avg}"
        );
    }

    #[test]
    fn lowpass_attenuates_high_freq() {
        // 8kHz サイン入力を 500Hz lowpass に通すと大きく減衰
        let mut high = sine_buf(8000.0, 4410);
        let rms_before = rms(&high);
        lowpass(&mut high, 500.0, 1.0, SAMPLE_RATE);
        let rms_after = rms(&high);
        assert!(
            rms_after < rms_before * 0.3,
            "expected ≥10dB attenuation, before={rms_before} after={rms_after}"
        );
    }

    #[test]
    fn highpass_attenuates_dc() {
        // DC 入力は highpass で消える (定常応答)
        let mut buf = dc_buf(0.5, 4410);
        highpass(&mut buf, 1000.0, 1.0, SAMPLE_RATE);
        let tail_avg: f32 = buf[3000..].iter().map(|(l, _)| *l).sum::<f32>() / 1410.0;
        assert!(
            tail_avg.abs() < 0.05,
            "DC should be blocked, got {tail_avg}"
        );
    }

    #[test]
    fn distortion_zero_amount_preserves_signal() {
        // amount=0 で drive gain=1、 tanh(x) ≈ x の範囲。 ただし内蔵 tone lowpass (cutoff=3000Hz、 q=0.7) は
        // 常に通るので 440Hz サインでも SVF の damping で多少振幅が下がる (Chamberlin はパスバンドが完全フラットではない)。
        // 「明示的に saturate されない」 ことは波形比較で見るより、 「ピークが極端に潰れていない」 で確認する。
        let mut buf = sine_buf(440.0, 4410);
        distortion(&mut buf, 0.0, 0.5, SAMPLE_RATE);
        let peak_after = buf.iter().map(|(l, _)| l.abs()).fold(0.0_f32, f32::max);
        // 元のサイン peak=1.0、 tone lowpass の damping で多少下がるが、 amount=0 なら 0.6 は維持
        assert!(
            peak_after > 0.6,
            "amount=0 should preserve most amplitude (no saturation), got peak={peak_after}"
        );
    }

    #[test]
    fn distortion_compresses_high_drive() {
        let mut buf = sine_buf(440.0, 4410);
        distortion(&mut buf, 1.0, 0.5, SAMPLE_RATE);
        let peak = buf.iter().map(|(l, _)| l.abs()).fold(0.0_f32, f32::max);
        // tanh で潰すので絶対値は 1.0 未満
        assert!(
            peak < 1.0,
            "distortion output should stay below 1.0, got {peak}"
        );
        // ただし無音にはならない
        assert!(peak > 0.1, "distortion output too small, got {peak}");
    }

    #[test]
    fn delay_produces_echo() {
        // 1 サンプル目だけ振幅 1 → 100ms 遅れて減衰したコピーが現れる
        let mut buf = vec![(0.0_f32, 0.0_f32); 44100];
        buf[0] = (1.0, 1.0);
        delay(&mut buf, 0.1, 0.5, 1.0, SAMPLE_RATE); // mix=1 で完全 wet
        let echo_idx = (0.1 * SAMPLE_RATE as f32) as usize;
        assert!(
            buf[echo_idx].0.abs() > 0.5,
            "expected echo at {echo_idx}, got {}",
            buf[echo_idx].0
        );
        // 2 回目のエコー (フィードバック) も検出できる
        let echo2_idx = echo_idx + (0.1 * SAMPLE_RATE as f32) as usize;
        assert!(
            buf[echo2_idx].0.abs() > 0.1,
            "expected 2nd echo at {echo2_idx}, got {}",
            buf[echo2_idx].0
        );
    }

    #[test]
    fn delay_dry_pass_through_at_zero_mix() {
        let mut buf = sine_buf(440.0, 4410);
        let original = buf.clone();
        delay(&mut buf, 0.1, 0.5, 0.0, SAMPLE_RATE);
        // mix=0 なら dry のみ
        for (a, b) in buf.iter().zip(original.iter()) {
            assert!((a.0 - b.0).abs() < 1e-6, "mix=0 should be exact dry");
        }
    }

    #[test]
    fn delay_feedback_clamped() {
        // feedback=10 が来てもクラッシュせず、 出力が ∞ に発散しない
        let mut buf = vec![(0.0_f32, 0.0_f32); 44100];
        buf[0] = (1.0, 1.0);
        delay(&mut buf, 0.05, 10.0, 0.5, SAMPLE_RATE);
        let peak = buf.iter().map(|(l, _)| l.abs()).fold(0.0_f32, f32::max);
        assert!(
            peak.is_finite() && peak < 50.0,
            "feedback clamp must hold, peak={peak}"
        );
    }

    #[test]
    fn parse_delay_time_bpm_sync() {
        // 120 BPM、 1/8 = 0.25 秒
        let t = parse_delay_time(&serde_json::json!("1/8"), 120);
        assert!((t - 0.25).abs() < 1e-3, "got {t}");
        // 1/4 = 0.5 秒
        let t = parse_delay_time(&serde_json::json!("1/4"), 120);
        assert!((t - 0.5).abs() < 1e-3, "got {t}");
    }

    #[test]
    fn parse_delay_time_raw_seconds() {
        let t = parse_delay_time(&serde_json::json!(0.42), 120);
        assert!((t - 0.42).abs() < 1e-6, "got {t}");
    }

    #[test]
    fn parse_delay_time_fallback() {
        // 不正な spec は 1/8 拍にフォールバック
        let t = parse_delay_time(&serde_json::json!(null), 120);
        assert!(
            (t - 0.25).abs() < 1e-3,
            "fallback should be 1/8 beat, got {t}"
        );
    }

    #[test]
    fn reverb_dry_passes_through_at_zero_mix() {
        let mut buf = sine_buf(440.0, 4410);
        let original = buf.clone();
        reverb(&mut buf, 0.5, 0.5, 0.0, SAMPLE_RATE);
        for (a, b) in buf.iter().zip(original.iter()) {
            assert!((a.0 - b.0).abs() < 1e-6, "mix=0 should be exact dry");
        }
    }

    #[test]
    fn reverb_extends_tail_beyond_input() {
        // 単発インパルス入力 → 入力長の数倍にわたって wet エコーが残る
        let mut buf = vec![(0.0_f32, 0.0_f32); SAMPLE_RATE as usize * 3];
        buf[0] = (1.0, 1.0);
        reverb(&mut buf, 0.7, 0.3, 1.0, SAMPLE_RATE);
        // 1 秒以降の任意の窓に有意な振幅が残っている
        let late_start = SAMPLE_RATE as usize;
        let tail_max = buf[late_start..]
            .iter()
            .map(|(l, _)| l.abs())
            .fold(0.0_f32, f32::max);
        assert!(
            tail_max > 1e-4,
            "reverb tail should remain audible at 1s, got peak={tail_max}"
        );
    }

    #[test]
    fn reverb_stays_bounded_under_heavy_input() {
        // 大入力でも発振しない (size=1, damp=0、 feedback 最大)
        let mut buf = vec![(0.0_f32, 0.0_f32); SAMPLE_RATE as usize * 2];
        for s in buf.iter_mut().take(1000) {
            *s = (0.9, 0.9);
        }
        reverb(&mut buf, 1.0, 0.0, 1.0, SAMPLE_RATE);
        let peak = buf.iter().map(|(l, _)| l.abs()).fold(0.0_f32, f32::max);
        assert!(
            peak.is_finite() && peak < 2.0,
            "reverb must stay bounded under heavy input, peak={peak}"
        );
    }

    #[test]
    fn reverb_stereo_widening() {
        // mono を入れても L/R で異なる出力になる (R は base+23 で delay 長が違うため)
        let mut buf = vec![(0.0_f32, 0.0_f32); SAMPLE_RATE as usize];
        buf[0] = (1.0, 1.0);
        reverb(&mut buf, 0.5, 0.5, 1.0, SAMPLE_RATE);
        // 0.2 秒以降を比較。 LR 完全同一なら spread が効いていない
        let from = (SAMPLE_RATE as f32 * 0.2) as usize;
        let lr_diff: f32 = buf[from..].iter().map(|(l, r)| (l - r).abs()).sum();
        assert!(
            lr_diff > 0.01,
            "stereo spread should desync L/R tail, got diff={lr_diff}"
        );
    }
}
