//! 0.1 → 0.2 schema migration。
//!
//! CDT-6 で導入。内蔵 synth (`sin` / `saw` / `square` / ...) を SF2 (`soundfont` type)
//! に LUT で置換する。設計: docs/design/03-cli.md (`migrate` 章) + ADR 周辺。
//!
//! 入出力は `serde_json::Value`。 `io::load` を経由しないのは、 CDT-7 で
//! `SUPPORTED_VERSIONS` を `["0.2"]` に絞った後でも 0.1 入力を受け付けられるようにするため。

use serde::Serialize;
use serde_json::{Map, Value};

/// LUT で楽器 type が見つからなかった時に当て込む default SF2 ファイル。
/// CLI / MCP の `--sf2` で上書き可能。
pub const DEFAULT_SF2: &str = "GeneralUser-GS.sf2";

const FROM_VERSION: &str = "0.1";
const TO_VERSION: &str = "0.2";

/// 0.1 内蔵 synth → SF2 preset/bank の対応表。
///
/// 未掲載 type は `None` (= caller 側で fallback warning + preset 0, bank 0)。
/// `soundfont` 既存 type はこの関数を呼ぶ前にスキップする。
fn lut(kind: &str) -> Option<(u16, u16)> {
    match kind {
        "sin" => Some((38, 0)),                    // Synth Bass 1
        "saw" | "saw_lead" => Some((81, 0)),       // Saw Lead
        "square" | "square_bass" => Some((80, 0)), // Square Lead
        "triangle" => Some((73, 0)),               // Flute
        "saw_pad" => Some((88, 0)),                // New Age Pad
        "drum_kit" => Some((0, 128)),              // Standard Drum Kit
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateOutcome {
    pub from_version: String,
    pub to_version: String,
    /// migrate 適用後の Song JSON。
    pub song: Value,
    /// 内蔵 synth → SF2 に置換された各 track の対応。
    /// `soundfont` 既存 track はここに含まれない。
    pub instrument_mapping: Vec<InstrumentMapping>,
    /// 非致命警告 (例: LUT 未掲載 type へ fallback 適用)。
    pub warnings: Vec<MigrateWarning>,
    /// `instrument_mapping.len()` と同値 (= LUT or fallback で書き換えた track 数)。
    pub tracks_migrated: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstrumentMapping {
    pub track_id: String,
    pub from_kind: String,
    pub to_kind: String,
    pub preset: u16,
    pub bank: u16,
    /// LUT 未掲載で preset 0 / bank 0 にフォールバックしたかどうか。
    pub fallback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrateWarning {
    pub code: &'static str,
    pub track_id: String,
    pub message: String,
}

#[derive(Debug)]
pub enum MigrateError {
    /// 入力 JSON が object でない / version key を持たない。
    MissingVersion,
    /// 既に target version (= 0.2)。 migrate 不要。
    NotNeeded { current: String },
    /// 0.1 でも 0.2 でもない未知 version。
    UnsupportedVersion(String),
    /// track が object でない / instrument key が欠落 / type が無い等。
    MalformedTrack { track_id: String, reason: String },
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingVersion => write!(f, "input has no top-level \"version\" field"),
            Self::NotNeeded { current } => {
                write!(
                    f,
                    "song is already at version {current}; migrate not needed"
                )
            }
            Self::UnsupportedVersion(v) => {
                write!(f, "unsupported source version {v:?}; expected \"0.1\"")
            }
            Self::MalformedTrack { track_id, reason } => {
                write!(f, "malformed track {track_id:?}: {reason}")
            }
        }
    }
}

impl std::error::Error for MigrateError {}

/// Song JSON (0.1) を 0.2 に変換する。
///
/// `sf2_file` 省略時は [`DEFAULT_SF2`] を使う。 入力 JSON は変更しない (clone)。
pub fn migrate_song_json(
    input: &Value,
    sf2_file: Option<&str>,
) -> Result<MigrateOutcome, MigrateError> {
    let sf2 = sf2_file.unwrap_or(DEFAULT_SF2);

    let current = input
        .as_object()
        .and_then(|o| o.get("version"))
        .and_then(|v| v.as_str())
        .ok_or(MigrateError::MissingVersion)?;

    if current == TO_VERSION {
        return Err(MigrateError::NotNeeded {
            current: current.to_string(),
        });
    }
    if current != FROM_VERSION {
        return Err(MigrateError::UnsupportedVersion(current.to_string()));
    }

    let mut out = input.clone();
    let out_obj = out.as_object_mut().expect("checked above");
    out_obj.insert("version".into(), Value::String(TO_VERSION.into()));

    let mut mapping = Vec::new();
    let mut warnings = Vec::new();

    if let Some(tracks) = out_obj.get_mut("tracks").and_then(|t| t.as_array_mut()) {
        for (idx, track) in tracks.iter_mut().enumerate() {
            let track_obj = track
                .as_object_mut()
                .ok_or_else(|| MigrateError::MalformedTrack {
                    track_id: format!("<index {idx}>"),
                    reason: "track is not a JSON object".into(),
                })?;

            let track_id = track_obj
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>")
                .to_string();

            let instrument =
                track_obj
                    .get_mut("instrument")
                    .ok_or_else(|| MigrateError::MalformedTrack {
                        track_id: track_id.clone(),
                        reason: "missing \"instrument\" key".into(),
                    })?;
            let inst_obj =
                instrument
                    .as_object_mut()
                    .ok_or_else(|| MigrateError::MalformedTrack {
                        track_id: track_id.clone(),
                        reason: "instrument is not a JSON object".into(),
                    })?;

            let kind = inst_obj
                .get("type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| MigrateError::MalformedTrack {
                    track_id: track_id.clone(),
                    reason: "instrument has no \"type\" field".into(),
                })?
                .to_string();

            // soundfont 既存はそのまま (mapping にも warning にも入れない)
            if kind == "soundfont" {
                continue;
            }

            let (preset, bank, fallback) = match lut(&kind) {
                Some((p, b)) => (p, b, false),
                None => {
                    warnings.push(MigrateWarning {
                        code: "MIGRATE_UNKNOWN_INSTRUMENT",
                        track_id: track_id.clone(),
                        message: format!(
                            "track '{track_id}' had instrument type '{kind}' with no LUT entry; \
                             defaulted to soundfont preset 0 (Acoustic Grand Piano). \
                             Review with set-instrument."
                        ),
                    });
                    (0, 0, true)
                }
            };

            // 旧 params (ADSR / pulse_width / kit 等) は破棄して新規 object を組み立てる
            let mut new_inst = Map::new();
            new_inst.insert("type".into(), Value::String("soundfont".into()));
            let mut params = Map::new();
            params.insert("file".into(), Value::String(sf2.to_string()));
            params.insert("preset".into(), Value::Number(u64::from(preset).into()));
            params.insert("bank".into(), Value::Number(u64::from(bank).into()));
            new_inst.insert("params".into(), Value::Object(params));
            *instrument = Value::Object(new_inst);

            mapping.push(InstrumentMapping {
                track_id: track_id.clone(),
                from_kind: kind,
                to_kind: "soundfont".to_string(),
                preset,
                bank,
                fallback,
            });
        }
    }

    let tracks_migrated = mapping.len();
    Ok(MigrateOutcome {
        from_version: FROM_VERSION.to_string(),
        to_version: TO_VERSION.to_string(),
        song: out,
        instrument_mapping: mapping,
        warnings,
        tracks_migrated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn song_with_instruments(kinds: &[&str]) -> Value {
        let tracks: Vec<Value> = kinds
            .iter()
            .enumerate()
            .map(|(i, k)| {
                json!({
                    "id": format!("t{i}"),
                    "name": format!("Track {i}"),
                    "instrument": { "type": k, "params": { "attack": 0.05 } },
                    "volume": 0.7,
                    "pan": 0.0,
                    "notes": [
                        { "t": 0.0, "pitch": "C4", "dur": 0.5, "vel": 100 }
                    ]
                })
            })
            .collect();
        json!({
            "version": "0.1",
            "metadata": { "name": "t", "bpm": 120 },
            "tracks": tracks,
        })
    }

    fn migrated_track_instrument(out: &MigrateOutcome, track_id: &str) -> Value {
        out.song["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == track_id)
            .expect("track")
            .get("instrument")
            .cloned()
            .expect("instrument")
    }

    #[test]
    fn lut_covers_all_six_entries() {
        let input = song_with_instruments(&[
            "sin",
            "saw",
            "saw_lead",
            "square",
            "square_bass",
            "triangle",
            "saw_pad",
            "drum_kit",
        ]);
        let out = migrate_song_json(&input, None).unwrap();

        assert_eq!(out.from_version, "0.1");
        assert_eq!(out.to_version, "0.2");
        assert_eq!(out.song["version"], "0.2");
        assert_eq!(out.tracks_migrated, 8);
        assert!(out.warnings.is_empty());

        let expected = [
            ("t0", "sin", 38_u16, 0_u16),
            ("t1", "saw", 81, 0),
            ("t2", "saw_lead", 81, 0),
            ("t3", "square", 80, 0),
            ("t4", "square_bass", 80, 0),
            ("t5", "triangle", 73, 0),
            ("t6", "saw_pad", 88, 0),
            ("t7", "drum_kit", 0, 128),
        ];
        for (track_id, from_kind, preset, bank) in expected {
            let m = out
                .instrument_mapping
                .iter()
                .find(|m| m.track_id == track_id)
                .unwrap_or_else(|| panic!("mapping for {track_id}"));
            assert_eq!(m.from_kind, from_kind);
            assert_eq!(m.to_kind, "soundfont");
            assert_eq!(m.preset, preset);
            assert_eq!(m.bank, bank);
            assert!(!m.fallback);

            let inst = migrated_track_instrument(&out, track_id);
            assert_eq!(inst["type"], "soundfont");
            assert_eq!(inst["params"]["file"], DEFAULT_SF2);
            assert_eq!(inst["params"]["preset"], preset);
            assert_eq!(inst["params"]["bank"], bank);
            // 旧 params (attack 等) は破棄されている
            assert!(inst["params"].get("attack").is_none());
        }
    }

    #[test]
    fn fallback_for_unknown_kind_emits_warning_and_uses_preset_zero() {
        let input = song_with_instruments(&["frobnicator"]);
        let out = migrate_song_json(&input, None).unwrap();

        assert_eq!(out.tracks_migrated, 1);
        let m = &out.instrument_mapping[0];
        assert_eq!(m.from_kind, "frobnicator");
        assert_eq!(m.preset, 0);
        assert_eq!(m.bank, 0);
        assert!(m.fallback);

        assert_eq!(out.warnings.len(), 1);
        let w = &out.warnings[0];
        assert_eq!(w.code, "MIGRATE_UNKNOWN_INSTRUMENT");
        assert_eq!(w.track_id, "t0");
        assert!(w.message.contains("frobnicator"));
        assert!(w.message.contains("preset 0"));
        assert!(w.message.contains("set-instrument"));
    }

    #[test]
    fn existing_soundfont_track_is_noop_and_excluded_from_mapping() {
        let input = json!({
            "version": "0.1",
            "metadata": { "name": "t", "bpm": 120 },
            "tracks": [
                {
                    "id": "sf",
                    "name": "Acoustic",
                    "instrument": {
                        "type": "soundfont",
                        "params": { "file": "custom.sf2", "preset": 24, "bank": 0 }
                    },
                    "volume": 0.7,
                    "pan": 0.0,
                    "notes": []
                },
                {
                    "id": "lead",
                    "name": "Lead",
                    "instrument": { "type": "sin" },
                    "volume": 0.7,
                    "pan": 0.0,
                    "notes": []
                }
            ]
        });
        let out = migrate_song_json(&input, None).unwrap();

        // soundfont track は instrument 構造ごと完全保持
        let sf_inst = migrated_track_instrument(&out, "sf");
        assert_eq!(sf_inst["type"], "soundfont");
        assert_eq!(sf_inst["params"]["file"], "custom.sf2");
        assert_eq!(sf_inst["params"]["preset"], 24);

        // mapping には sin の 1 track だけ
        assert_eq!(out.tracks_migrated, 1);
        assert_eq!(out.instrument_mapping.len(), 1);
        assert_eq!(out.instrument_mapping[0].track_id, "lead");
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn already_v02_returns_not_needed() {
        let input = json!({
            "version": "0.2",
            "metadata": { "name": "t", "bpm": 120 },
            "tracks": []
        });
        let err = migrate_song_json(&input, None).unwrap_err();
        assert!(matches!(err, MigrateError::NotNeeded { current } if current == "0.2"));
    }

    #[test]
    fn unknown_version_is_unsupported() {
        let input = json!({
            "version": "9.9",
            "metadata": { "name": "t", "bpm": 120 },
            "tracks": []
        });
        let err = migrate_song_json(&input, None).unwrap_err();
        assert!(matches!(err, MigrateError::UnsupportedVersion(v) if v == "9.9"));
    }

    #[test]
    fn missing_version_is_error() {
        let input = json!({ "metadata": { "name": "t", "bpm": 120 } });
        let err = migrate_song_json(&input, None).unwrap_err();
        assert!(matches!(err, MigrateError::MissingVersion));
    }

    #[test]
    fn sf2_file_override_propagates_to_params() {
        let input = song_with_instruments(&["sin"]);
        let out = migrate_song_json(&input, Some("MyKit.sf2")).unwrap();
        let inst = migrated_track_instrument(&out, "t0");
        assert_eq!(inst["params"]["file"], "MyKit.sf2");
    }
}
