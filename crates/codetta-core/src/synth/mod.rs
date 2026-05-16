//! Phase 0 first cut の最小シンセエンジン。
//!
//! 信号フローは docs/design/05-sound.md 参照。 現スコープは
//! `sin` オシレータ + ADSR のみ。 フィルタ / ドラム / 他のオシレータは続く実装で。
//!
//! `fundsp` 採用判断結果: 不採用 (docs/design/05-sound.md 参照)。 自前ループ
//! 実装 [`manual`] で進める。

pub mod manual;

/// 標準サンプルレート。 Phase 0 では 44.1kHz 固定 (05-sound.md)。
pub const SAMPLE_RATE: u32 = 44100;

/// ADSR エンベロープのパラメータ (秒、 sustain は 0-1)。
/// デフォルトは 05-sound.md「ADSR エンベロープ」表の値。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdsrParams {
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
}

impl Default for AdsrParams {
    fn default() -> Self {
        Self {
            attack: 0.01,
            decay: 0.1,
            sustain: 0.7,
            release: 0.2,
        }
    }
}

impl AdsrParams {
    /// `Instrument.params` の JSON Map から ADSR を取り出す。 未指定はデフォルト。
    pub fn from_params(params: &serde_json::Map<String, serde_json::Value>) -> Self {
        let mut p = Self::default();
        if let Some(v) = params.get("attack").and_then(|v| v.as_f64()) {
            p.attack = v as f32;
        }
        if let Some(v) = params.get("decay").and_then(|v| v.as_f64()) {
            p.decay = v as f32;
        }
        if let Some(v) = params.get("sustain").and_then(|v| v.as_f64()) {
            p.sustain = (v as f32).clamp(0.0, 1.0);
        }
        if let Some(v) = params.get("release").and_then(|v| v.as_f64()) {
            p.release = v as f32;
        }
        p
    }
}

/// MIDI ノート番号 → 周波数 (Hz)。 A4 (69) = 440 Hz。
pub fn midi_to_freq(midi: u8) -> f32 {
    440.0 * 2.0_f32.powf((midi as f32 - 69.0) / 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn adsr_defaults() {
        let p = AdsrParams::from_params(&serde_json::Map::new());
        assert_eq!(p, AdsrParams::default());
    }

    #[test]
    fn adsr_from_json() {
        let v = json!({"attack": 0.5, "sustain": 0.3, "release": 1.5});
        let p = AdsrParams::from_params(v.as_object().unwrap());
        assert!((p.attack - 0.5).abs() < 1e-6);
        assert!((p.sustain - 0.3).abs() < 1e-6);
        assert!((p.release - 1.5).abs() < 1e-6);
        // decay は未指定なのでデフォルト
        assert!((p.decay - AdsrParams::default().decay).abs() < 1e-6);
    }

    #[test]
    fn adsr_clamps_sustain() {
        let v = json!({"sustain": 2.0});
        let p = AdsrParams::from_params(v.as_object().unwrap());
        assert_eq!(p.sustain, 1.0);
    }

    #[test]
    fn midi_to_freq_a4() {
        assert!((midi_to_freq(69) - 440.0).abs() < 1e-3);
        assert!((midi_to_freq(60) - 261.6256).abs() < 1e-2);
    }
}
