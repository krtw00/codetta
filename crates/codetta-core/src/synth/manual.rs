//! 自前ループ実装の 1 voice (オシレータ + ADSR)。
//!
//! `fundsp` を使わずに済むかを判断するための「自前版」プロトタイプ。
//! 現状は `sin` / `saw` / `square` (PolyBLEP) / `triangle` (解析式) / `saw_pad` (saw × 3 detune) を実装済み。
//!
//! Envelope 曲線は線形セグメント。 sound.md は「指数」を最終形と書いているが、
//! Phase 0 first cut は線形で十分音になる (差し替えは Phase 1+)。

use std::f32::consts::{PI, TAU};

use super::{AdsrParams, SAMPLE_RATE};

/// 決定論的 xorshift32 (drum voice の noise 用)。 同じ seed で常に同じ noise を返すので、
/// テストで波形を比較できる。
struct Xorshift32 {
    state: u32,
}

impl Xorshift32 {
    fn new(seed: u32) -> Self {
        Self {
            state: seed.max(1),
        }
    }
    fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }
    /// -1.0..1.0 の white noise。
    fn noise(&mut self) -> f32 {
        (self.next_u32() as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// 1 極 IIR highpass を 1 サンプルだけ進める (RC ハイパス相当)。
/// `state.0` は直前の入力、 `state.1` は直前の出力。
fn one_pole_highpass_tick(state: &mut (f32, f32), input: f32, cutoff_hz: f32, sr: f32) -> f32 {
    let alpha = 2.0 * PI * cutoff_hz / sr;
    let a = 1.0 / (1.0 + alpha);
    let out = a * (state.1 + input - state.0);
    state.0 = input;
    state.1 = out;
    out
}

/// 1 極 IIR lowpass を 1 サンプルだけ進める。 `state` は直前の出力。
fn one_pole_lowpass_tick(state: &mut f32, input: f32, cutoff_hz: f32, sr: f32) -> f32 {
    let alpha = (2.0 * PI * cutoff_hz / sr).min(1.0);
    *state += alpha * (input - *state);
    *state
}

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

/// 1 ノート分の kick (バスドラム) を生成する。 sin の pitch sweep + amp envelope + click noise。
///
/// `kit` で `808` / `909` 系のバリエーションに切り替え可能 (sound.md 仕様)。
/// 不明 / 未指定なら `default` レシピ。 波形長は kit ごとの amp_decay + 50ms tail で自然消滅する。
/// `chip` は sin/click ベースから外れる別形式 (8bit 風 square sweep) なので [`render_drum_kick_chip`] に dispatch。
///
/// note の velocity は呼び出し側 (render) で乗算するので、 ここではピーク 1.0 近辺の波形を返す。
pub fn render_drum_kick(kit: Option<&str>) -> Vec<f32> {
    if kit == Some("chip") {
        return render_drum_kick_chip();
    }
    let sr = SAMPLE_RATE as f32;
    // (f_start, f_end, pitch_decay_sec, amp_decay_sec, click_gain)
    let (f_start, f_end, pitch_decay, amp_decay, click_gain) = match kit.unwrap_or("default") {
        "808" => (80.0_f32, 40.0_f32, 0.1_f32, 0.4_f32, 0.1_f32),
        "909" => (200.0_f32, 60.0_f32, 0.04_f32, 0.2_f32, 0.4_f32),
        _ => (150.0_f32, 50.0_f32, 0.05_f32, 0.2_f32, 0.25_f32),
    };
    let total = ((amp_decay + 0.05) * sr) as usize;
    let mut out = Vec::with_capacity(total);
    let mut phase = 0.0_f32;
    let mut rng = Xorshift32::new(0xDEAD_BEEF);
    let click_samples = (0.005 * sr) as usize;
    let attack_samples = ((0.001 * sr) as usize).max(1);
    let amp_tau = amp_decay / 3.0; // 3 倍の時定数で ~5% まで減衰
    for i in 0..total {
        let t = i as f32 / sr;
        let pitch_env = (-t / pitch_decay).exp();
        let freq = f_end + (f_start - f_end) * pitch_env;
        phase += TAU * freq / sr;
        if phase > TAU {
            phase -= TAU;
        }
        let sin = phase.sin();
        let amp = if i < attack_samples {
            i as f32 / attack_samples as f32
        } else {
            (-(t - attack_samples as f32 / sr) / amp_tau).exp()
        };
        let click = if i < click_samples {
            rng.noise() * click_gain * (1.0 - i as f32 / click_samples as f32)
        } else {
            0.0
        };
        out.push(((sin * amp) + click).clamp(-1.0, 1.0));
    }
    out
}

/// 1 ノート分の snare (スネア) を生成する。 sin 200Hz + bandpass された noise の合成。
///
/// bandpass は 1 極 highpass (1.5kHz) + 1 極 lowpass (4kHz) を直列にした簡易版。
/// `kit` で `808` (sin 主体、 長め) / `909` (noise 主体、 短め) に切り替え。
/// `chip` は 1bit noise のみで sin を持たない別形式なので [`render_drum_snare_chip`] に dispatch。
pub fn render_drum_snare(kit: Option<&str>) -> Vec<f32> {
    if kit == Some("chip") {
        return render_drum_snare_chip();
    }
    let sr = SAMPLE_RATE as f32;
    let (sin_gain, noise_gain, decay) = match kit.unwrap_or("default") {
        "808" => (0.7_f32, 0.4_f32, 0.25_f32),
        "909" => (0.4_f32, 0.8_f32, 0.12_f32),
        _ => (0.5_f32, 0.6_f32, 0.15_f32),
    };
    let total = ((decay + 0.05) * sr) as usize;
    let mut out = Vec::with_capacity(total);
    let mut phase = 0.0_f32;
    let sin_inc = TAU * 200.0 / sr;
    let mut rng = Xorshift32::new(0xCAFE_BABE);
    let mut hp = (0.0_f32, 0.0_f32);
    let mut lp = 0.0_f32;
    let attack_samples = ((0.001 * sr) as usize).max(1);
    let amp_tau = decay / 3.0;
    for i in 0..total {
        phase += sin_inc;
        if phase > TAU {
            phase -= TAU;
        }
        let sin = phase.sin();
        let raw_noise = rng.noise();
        let hp_out = one_pole_highpass_tick(&mut hp, raw_noise, 1500.0, sr);
        let bp = one_pole_lowpass_tick(&mut lp, hp_out, 4000.0, sr);
        let t = i as f32 / sr;
        let amp = if i < attack_samples {
            i as f32 / attack_samples as f32
        } else {
            (-(t - attack_samples as f32 / sr) / amp_tau).exp()
        };
        out.push(((sin * sin_gain + bp * noise_gain) * amp).clamp(-1.0, 1.0));
    }
    out
}

/// chip 風 kick。 naive square (PolyBLEP 補正なし) の pitch sweep + 短い amp decay。
///
/// ファミコンの pulse channel ぽい「コッ」 / 「ポッ」 感を出すため、 sin ではなく 50% duty square を
/// 直接走らせる。 エイリアシングは「8bit 感」 として許容する (sound.md の chip kit 方針)。
/// pitch sweep は 200Hz → 60Hz exp (decay 25ms)、 amp は 80ms exp。 click noise は持たない。
fn render_drum_kick_chip() -> Vec<f32> {
    let sr = SAMPLE_RATE as f32;
    let pitch_decay = 0.025_f32;
    let amp_decay = 0.08_f32;
    let f_start = 200.0_f32;
    let f_end = 60.0_f32;
    let total = ((amp_decay + 0.05) * sr) as usize;
    let mut out = Vec::with_capacity(total);
    let mut phase = 0.0_f32; // 0..1
    let attack_samples = ((0.001 * sr) as usize).max(1);
    let amp_tau = amp_decay / 3.0;
    for i in 0..total {
        let t = i as f32 / sr;
        let pitch_env = (-t / pitch_decay).exp();
        let freq = f_end + (f_start - f_end) * pitch_env;
        phase += freq / sr;
        if phase >= 1.0 {
            phase -= 1.0;
        }
        let sq = if phase < 0.5 { 1.0 } else { -1.0 };
        let amp = if i < attack_samples {
            i as f32 / attack_samples as f32
        } else {
            (-(t - attack_samples as f32 / sr) / amp_tau).exp()
        };
        out.push((sq * amp).clamp(-1.0, 1.0));
    }
    out
}

/// chip 風 snare。 1bit noise (xorshift32 の sign のみ) を highpass で薄めて短い decay。
///
/// NES noise channel の long-mode (15bit LFSR) を近似する用途ではなく、 「ザッ」 という lo-fi
/// 矩形ノイズ感を出す簡易版。 sin 成分は持たない (chip kit の snare は noise 主体)。
fn render_drum_snare_chip() -> Vec<f32> {
    let sr = SAMPLE_RATE as f32;
    let decay = 0.06_f32;
    let total = ((decay + 0.05) * sr) as usize;
    let mut out = Vec::with_capacity(total);
    let mut rng = Xorshift32::new(0xB16B_00B5);
    let mut hp = (0.0_f32, 0.0_f32);
    let attack_samples = ((0.001 * sr) as usize).max(1);
    let amp_tau = decay / 3.0;
    for i in 0..total {
        // 1bit 量子化 (sign 関数)
        let bit = if rng.noise() >= 0.0 { 1.0 } else { -1.0 };
        let h = one_pole_highpass_tick(&mut hp, bit, 2000.0, sr);
        let t = i as f32 / sr;
        let amp = if i < attack_samples {
            i as f32 / attack_samples as f32
        } else {
            (-(t - attack_samples as f32 / sr) / amp_tau).exp()
        };
        out.push((h * amp).clamp(-1.0, 1.0));
    }
    out
}

/// 1 ノート分のハイハットを生成する。 highpass 6kHz された white noise + amp envelope。
/// `open=false` で closed (50ms decay)、 `open=true` で open (500ms decay)。
pub fn render_drum_hh(open: bool) -> Vec<f32> {
    let sr = SAMPLE_RATE as f32;
    let decay = if open { 0.5 } else { 0.05 };
    let total = ((decay + 0.05) * sr) as usize;
    let mut out = Vec::with_capacity(total);
    let mut rng = Xorshift32::new(if open { 0x1234_5678 } else { 0x8765_4321 });
    let mut hp = (0.0_f32, 0.0_f32);
    let attack_samples = ((0.001 * sr) as usize).max(1);
    let amp_tau = decay / 3.0;
    for i in 0..total {
        let raw = rng.noise();
        let h = one_pole_highpass_tick(&mut hp, raw, 6000.0, sr);
        let t = i as f32 / sr;
        let amp = if i < attack_samples {
            i as f32 / attack_samples as f32
        } else {
            (-(t - attack_samples as f32 / sr) / amp_tau).exp()
        };
        out.push((h * amp).clamp(-1.0, 1.0));
    }
    out
}

/// 1 ノート分の clap を生成する。 bandpass noise を 4 連バーストで重ねた典型的なリズムマシン clap。
pub fn render_drum_clap() -> Vec<f32> {
    let sr = SAMPLE_RATE as f32;
    let total_sec = 0.3_f32;
    let total = (total_sec * sr) as usize;
    let mut out = Vec::with_capacity(total);
    let mut rng = Xorshift32::new(0x00C0_FFEE);
    let mut hp = (0.0_f32, 0.0_f32);
    let mut lp = 0.0_f32;
    // 短い 4 バースト @ 0/10/20/30ms + 全体に長めの tail
    let burst_offsets_sec = [0.0_f32, 0.010, 0.020, 0.030];
    let burst_tau = 0.008_f32;
    let tail_tau = 0.05_f32;
    for i in 0..total {
        let raw = rng.noise();
        let hp_out = one_pole_highpass_tick(&mut hp, raw, 1500.0, sr);
        let bp = one_pole_lowpass_tick(&mut lp, hp_out, 4000.0, sr);
        let t = i as f32 / sr;
        let mut burst_env = 0.0_f32;
        for &off in &burst_offsets_sec {
            if t >= off {
                burst_env += (-(t - off) / burst_tau).exp();
            }
        }
        let tail_env = (-t / tail_tau).exp();
        let amp = (burst_env * 0.4 + tail_env * 0.3).min(1.0);
        out.push((bp * amp).clamp(-1.0, 1.0));
    }
    out
}

/// 1 ノート分の crash を生成する。 bandpass (4-10kHz) noise + 長い decay (2s)。
pub fn render_drum_crash() -> Vec<f32> {
    let sr = SAMPLE_RATE as f32;
    let decay = 2.0_f32;
    let total = ((decay + 0.1) * sr) as usize;
    let mut out = Vec::with_capacity(total);
    let mut rng = Xorshift32::new(0xCABA_DAFE);
    let mut hp = (0.0_f32, 0.0_f32);
    let mut lp = 0.0_f32;
    let attack_samples = ((0.005 * sr) as usize).max(1);
    let amp_tau = decay / 3.0;
    for i in 0..total {
        let raw = rng.noise();
        let hp_out = one_pole_highpass_tick(&mut hp, raw, 4000.0, sr);
        let bp = one_pole_lowpass_tick(&mut lp, hp_out, 10000.0, sr);
        let t = i as f32 / sr;
        let amp = if i < attack_samples {
            i as f32 / attack_samples as f32
        } else {
            (-(t - attack_samples as f32 / sr) / amp_tau).exp()
        };
        out.push((bp * amp).clamp(-1.0, 1.0));
    }
    out
}

/// 1 ノート分の ride を生成する。 bandpass (5-8kHz) noise + 中程度の decay (1s) + sin 800Hz の ping。
pub fn render_drum_ride() -> Vec<f32> {
    let sr = SAMPLE_RATE as f32;
    let decay = 1.0_f32;
    let total = ((decay + 0.1) * sr) as usize;
    let mut out = Vec::with_capacity(total);
    let mut rng = Xorshift32::new(0xFADE_C0DE);
    let mut hp = (0.0_f32, 0.0_f32);
    let mut lp = 0.0_f32;
    let attack_samples = ((0.002 * sr) as usize).max(1);
    let amp_tau = decay / 3.0;
    let mut phase = 0.0_f32;
    let ping_inc = TAU * 800.0 / sr;
    let ping_tau = 0.04_f32;
    for i in 0..total {
        let raw = rng.noise();
        let hp_out = one_pole_highpass_tick(&mut hp, raw, 5000.0, sr);
        let bp = one_pole_lowpass_tick(&mut lp, hp_out, 8000.0, sr);
        phase += ping_inc;
        if phase > TAU {
            phase -= TAU;
        }
        let t = i as f32 / sr;
        let amp = if i < attack_samples {
            i as f32 / attack_samples as f32
        } else {
            (-(t - attack_samples as f32 / sr) / amp_tau).exp()
        };
        let ping_env = (-t / ping_tau).exp();
        out.push(((bp * 0.7 + phase.sin() * ping_env * 0.3) * amp).clamp(-1.0, 1.0));
    }
    out
}

/// 1 ノート分の tom を生成する。 sin の pitch sweep + 短い click noise。
/// `base_freq_hz` で lo/mid/hi の音高差を作る (80/150/220 Hz 推奨)。
pub fn render_drum_tom(base_freq_hz: f32) -> Vec<f32> {
    let sr = SAMPLE_RATE as f32;
    let decay = 0.4_f32;
    let total = ((decay + 0.05) * sr) as usize;
    let mut out = Vec::with_capacity(total);
    let mut phase = 0.0_f32;
    let mut rng = Xorshift32::new(0x704D_F00D);
    let f_start = base_freq_hz * 1.5;
    let f_end = base_freq_hz;
    let pitch_decay = 0.03_f32;
    let click_samples = (0.003 * sr) as usize;
    let attack_samples = ((0.001 * sr) as usize).max(1);
    let amp_tau = decay / 3.0;
    for i in 0..total {
        let t = i as f32 / sr;
        let pitch_env = (-t / pitch_decay).exp();
        let freq = f_end + (f_start - f_end) * pitch_env;
        phase += TAU * freq / sr;
        if phase > TAU {
            phase -= TAU;
        }
        let sin = phase.sin();
        let click = if i < click_samples {
            rng.noise() * 0.3 * (1.0 - i as f32 / click_samples as f32)
        } else {
            0.0
        };
        let amp = if i < attack_samples {
            i as f32 / attack_samples as f32
        } else {
            (-(t - attack_samples as f32 / sr) / amp_tau).exp()
        };
        out.push(((sin + click) * amp).clamp(-1.0, 1.0));
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

    // drum_kit: 「振幅が ±1 内 + 各 voice が無音にならない + 異なる drum_key で違う波形」 の 3 軸テスト。
    // 音色の良し悪し (RT60 / spectrum) は耳テスト前提なので、 機械的に確認できる範囲だけにとどめる。

    fn span(v: &[f32]) -> f32 {
        v.iter().cloned().fold(0.0_f32, f32::max) - v.iter().cloned().fold(0.0_f32, f32::min)
    }

    fn peak(v: &[f32]) -> f32 {
        v.iter().map(|x| x.abs()).fold(0.0_f32, f32::max)
    }

    #[test]
    fn drum_kick_produces_audio_and_stays_bounded() {
        let v = render_drum_kick(None);
        assert!(!v.is_empty());
        let p = peak(&v);
        assert!(p > 0.3, "kick should be audible: {p}");
        assert!(p <= 1.0, "kick should stay bounded: {p}");
    }

    #[test]
    fn drum_kick_kit_variations_produce_distinct_lengths() {
        let d = render_drum_kick(None);
        let a808 = render_drum_kick(Some("808"));
        let a909 = render_drum_kick(Some("909"));
        // 808 は decay 400ms で最も長く、 909 は 200ms、 default は 200ms
        assert!(a808.len() > d.len(), "808 should be longer than default: 808={} default={}", a808.len(), d.len());
        assert_eq!(a909.len(), d.len(), "909 and default share 200ms decay, lengths should match");
    }

    #[test]
    fn drum_snare_produces_audio_and_stays_bounded() {
        let v = render_drum_snare(None);
        let p = peak(&v);
        assert!(p > 0.2, "snare should be audible: {p}");
        assert!(p <= 1.0, "snare should stay bounded: {p}");
    }

    #[test]
    fn drum_hh_closed_is_shorter_than_open() {
        let closed = render_drum_hh(false);
        let open = render_drum_hh(true);
        assert!(open.len() > closed.len() * 5, "open hh should be ~10x longer than closed: closed={} open={}", closed.len(), open.len());
        assert!(peak(&closed) > 0.1 && peak(&closed) <= 1.0);
        assert!(peak(&open) > 0.1 && peak(&open) <= 1.0);
    }

    #[test]
    fn drum_clap_crash_ride_each_make_sound() {
        for (name, v) in [
            ("clap", render_drum_clap()),
            ("crash", render_drum_crash()),
            ("ride", render_drum_ride()),
        ] {
            let p = peak(&v);
            assert!(p > 0.1, "{name} should be audible: {p}");
            assert!(p <= 1.0, "{name} should stay bounded: {p}");
        }
    }

    #[test]
    fn drum_tom_pitch_distinguishes_lo_mid_hi() {
        // 同じ duration だが波形は周波数で異なる
        let lo = render_drum_tom(80.0);
        let mid = render_drum_tom(150.0);
        let hi = render_drum_tom(220.0);
        assert_eq!(lo.len(), mid.len());
        assert_eq!(mid.len(), hi.len());
        // 波形が一致しないこと (= 違う freq で違う sin が出ている)
        let diff_lo_hi: usize = lo
            .iter()
            .zip(hi.iter())
            .filter(|(a, b)| (*a - *b).abs() > 0.01)
            .count();
        assert!(diff_lo_hi > 100, "lo and hi toms should differ: diff={diff_lo_hi}");
        for v in [&lo, &mid, &hi] {
            assert!(peak(v) > 0.2 && peak(v) <= 1.0);
        }
    }

    #[test]
    fn drum_kick_chip_distinct_from_default() {
        // chip kit は別関数に dispatch されるので、 default / 808 / 909 のいずれとも波形が一致しない
        let chip = render_drum_kick(Some("chip"));
        assert!(!chip.is_empty());
        let p = peak(&chip);
        assert!(p > 0.3, "chip kick should be audible: {p}");
        assert!(p <= 1.0, "chip kick should stay bounded: {p}");
        for other_kit in [None, Some("808"), Some("909")] {
            let other = render_drum_kick(other_kit);
            // 長さか中身のどちらかが必ず違う
            let differs = chip.len() != other.len()
                || chip
                    .iter()
                    .zip(other.iter())
                    .filter(|(a, b)| (*a - *b).abs() > 0.01)
                    .count()
                    > 100;
            assert!(differs, "chip kick should differ from kit={other_kit:?}");
        }
    }

    #[test]
    fn drum_snare_chip_distinct_from_default() {
        let chip = render_drum_snare(Some("chip"));
        assert!(!chip.is_empty());
        let p = peak(&chip);
        assert!(p > 0.1, "chip snare should be audible: {p}");
        assert!(p <= 1.0, "chip snare should stay bounded: {p}");
        for other_kit in [None, Some("808"), Some("909")] {
            let other = render_drum_snare(other_kit);
            let differs = chip.len() != other.len()
                || chip
                    .iter()
                    .zip(other.iter())
                    .filter(|(a, b)| (*a - *b).abs() > 0.01)
                    .count()
                    > 100;
            assert!(differs, "chip snare should differ from kit={other_kit:?}");
        }
    }

    #[test]
    fn drum_chip_voices_have_finite_tail() {
        for (name, v) in [
            ("chip_kick", render_drum_kick(Some("chip"))),
            ("chip_snare", render_drum_snare(Some("chip"))),
        ] {
            let tail_start = v.len().saturating_sub(SAMPLE_RATE as usize / 200); // 末尾 5ms
            let tail = &v[tail_start..];
            let head_peak = peak(&v[..v.len() / 2]);
            let tail_peak = peak(tail);
            assert!(
                tail_peak < head_peak * 0.5,
                "{name} tail should be quieter than head: head={head_peak} tail={tail_peak}"
            );
        }
    }

    #[test]
    fn drum_voices_have_finite_tail() {
        // どの voice も末尾近辺で減衰している (= 自然消滅する)
        for (name, v) in [
            ("kick", render_drum_kick(None)),
            ("snare", render_drum_snare(None)),
            ("hh_closed", render_drum_hh(false)),
            ("crash", render_drum_crash()),
            ("tom_lo", render_drum_tom(80.0)),
        ] {
            let tail_start = v.len().saturating_sub(SAMPLE_RATE as usize / 200); // 末尾 5ms
            let tail = &v[tail_start..];
            let head_peak = peak(&v[..v.len() / 2]);
            let tail_peak = peak(tail);
            assert!(
                tail_peak < head_peak * 0.5,
                "{name} tail should be quieter than head: head={head_peak} tail={tail_peak}"
            );
        }
        // 上で使った span は dead code 警告回避のため touch
        let _ = span(&render_drum_kick(None));
    }
}
