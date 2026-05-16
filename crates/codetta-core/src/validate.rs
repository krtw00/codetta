use std::collections::HashSet;

use crate::error::ValidationError;
use crate::model::{Pitch, Song};
use crate::synth::soundfont::{resolve_soundfont_path, SoundFontParams};

/// 既知のシンセ / ドラム楽器 type。
pub const KNOWN_INSTRUMENT_TYPES: &[&str] = &[
    "sin",
    "saw",
    "saw_lead",
    "square",
    "square_bass",
    "triangle",
    "saw_pad",
    "drum_kit",
    "soundfont",
];

/// 既知のエフェクト type (Phase 0)。
pub const KNOWN_EFFECT_TYPES: &[&str] = &["lowpass", "highpass", "delay", "reverb", "distortion"];

/// drum_kit トラックで使えるピッチ名 (GM Drum 互換キー)。
pub const KNOWN_DRUM_KEYS: &[&str] = &[
    "kick",
    "snare",
    "hh_closed",
    "hh_open",
    "clap",
    "crash",
    "ride",
    "tom_lo",
    "tom_mid",
    "tom_hi",
];

/// ADSR 共通パラメータ。 オシレータ系で共有。
const ADSR_PARAM_KEYS: &[&str] = &["attack", "decay", "sustain", "release"];

/// 楽器 type ごとに「認識される (= レンダリングで実際に使われる) param キー一覧」を返す。
///
/// 未知 type は `None`。 認識外キーは validate で `UNKNOWN_PARAM` Warning として報告する。
/// 追記時の責務: `crates/codetta-cli/src/main.rs::instrument_catalog()` の params keys と
/// 一致させる (sync test `catalog_params_match_instrument_param_keys` で保証)。
pub fn instrument_param_keys(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "sin" | "saw" | "saw_lead" | "triangle" => Some(ADSR_PARAM_KEYS),
        "square" | "square_bass" => Some(&["attack", "decay", "sustain", "release", "pulse_width"]),
        "saw_pad" => Some(&["attack", "decay", "sustain", "release", "detune_cents"]),
        "drum_kit" => Some(&["kit"]),
        "soundfont" => Some(&["file", "preset", "bank"]),
        _ => None,
    }
}

/// エフェクト type ごとに「認識される (= レンダリングで実際に使われる) param キー一覧」を返す。
///
/// 未知 type は `None`。 認識外キーは validate で `UNKNOWN_PARAM` Warning として報告する。
/// 追記時の責務: `crates/codetta-cli/src/main.rs::effect_catalog()` の params keys と
/// 一致させる (sync test `catalog_params_match_effect_param_keys` で保証)。
pub fn effect_param_keys(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "lowpass" | "highpass" => Some(&["cutoff", "q"]),
        "distortion" => Some(&["amount", "tone"]),
        "delay" => Some(&["time", "feedback", "mix"]),
        "reverb" => Some(&["size", "damp", "mix"]),
        _ => None,
    }
}

/// 楽曲全体を検証し、 違反を列挙する。 空 Vec なら整合性 OK。
///
/// version の妥当性は `io::load` で既にチェックされている前提だが、
/// in-memory 構築されたケース向けにここでも再チェックする。
pub fn validate(song: &Song) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // version
    if !crate::SUPPORTED_VERSIONS.contains(&song.version.as_str()) {
        errors.push(ValidationError::new(
            "UNKNOWN_VERSION",
            "version",
            format!("unsupported schema version: {:?}", song.version),
        ));
    }

    // metadata
    let bpm = song.metadata.bpm;
    if !(20..=300).contains(&bpm) {
        errors.push(ValidationError::new(
            "INVALID_SCHEMA",
            "metadata.bpm",
            format!("bpm must be in 20..=300, got {bpm}"),
        ));
    }
    let [_, denom] = song.metadata.time_signature;
    if denom == 0 || (denom & (denom - 1)) != 0 {
        errors.push(ValidationError::new(
            "INVALID_SCHEMA",
            "metadata.time_signature",
            format!("denominator must be a power of two, got {denom}"),
        ));
    }
    let mg = song.metadata.master_gain;
    if !mg.is_finite() || !(0.0..=4.0).contains(&mg) {
        errors.push(ValidationError::new(
            "INVALID_SCHEMA",
            "metadata.master_gain",
            format!("master_gain must be a finite number in 0.0..=4.0, got {mg}"),
        ));
    }

    // tracks
    let mut seen_ids: HashSet<&str> = HashSet::new();
    for (ti, track) in song.tracks.iter().enumerate() {
        let tprefix = format!("tracks[{ti}]");

        if track.id.is_empty() {
            errors.push(ValidationError::new(
                "INVALID_SCHEMA",
                format!("{tprefix}.id"),
                "track id must be non-empty",
            ));
        } else if !seen_ids.insert(track.id.as_str()) {
            errors.push(ValidationError::new(
                "TRACK_ID_DUPLICATE",
                format!("{tprefix}.id"),
                format!("duplicate track id: {:?}", track.id),
            ));
        }

        if !(0.0..=1.0).contains(&track.volume) {
            errors.push(ValidationError::new(
                "INVALID_SCHEMA",
                format!("{tprefix}.volume"),
                format!("volume must be in 0.0..=1.0, got {}", track.volume),
            ));
        }
        if !(-1.0..=1.0).contains(&track.pan) {
            errors.push(ValidationError::new(
                "INVALID_SCHEMA",
                format!("{tprefix}.pan"),
                format!("pan must be in -1.0..=1.0, got {}", track.pan),
            ));
        }

        // instrument
        if !KNOWN_INSTRUMENT_TYPES.contains(&track.instrument.kind.as_str()) {
            errors.push(ValidationError::new(
                "UNKNOWN_INSTRUMENT_TYPE",
                format!("{tprefix}.instrument.type"),
                format!("unknown instrument type: {:?}", track.instrument.kind),
            ));
        }
        let is_drum = track.instrument.kind == "drum_kit";

        // soundfont params: file/preset/bank の型 + 解決後 path の存在を確認
        if track.instrument.kind == "soundfont" {
            match SoundFontParams::from_params(&track.instrument.params) {
                Err(e) => errors.push(ValidationError::new(
                    "INVALID_SCHEMA",
                    format!("{tprefix}.instrument.params"),
                    e.to_string(),
                )),
                Ok(sf) => {
                    let resolved = resolve_soundfont_path(&sf.file);
                    if !resolved.exists() {
                        errors.push(ValidationError::new(
                            "SOUNDFONT_FILE_NOT_FOUND",
                            format!("{tprefix}.instrument.params.file"),
                            format!(
                                "soundfont file not found: {} (resolved from {:?}, set $CODETTA_SOUNDFONT_DIR or use absolute path)",
                                resolved.display(),
                                sf.file
                            ),
                        ));
                    }
                }
            }
        }

        // instrument params: 認識されない (= レンダリングで無視される) キーを警告
        if let Some(known) = instrument_param_keys(&track.instrument.kind) {
            for key in track.instrument.params.keys() {
                if !known.iter().any(|k| k == key) {
                    errors.push(ValidationError::warning(
                        "UNKNOWN_PARAM",
                        format!("{tprefix}.instrument.params.{key}"),
                        format!(
                            "param {key:?} is not recognized by instrument {:?} and will be ignored at render time (known params: {known:?})",
                            track.instrument.kind
                        ),
                    ));
                }
            }
        }

        // effects
        for (fi, fx) in track.fx.iter().enumerate() {
            if !KNOWN_EFFECT_TYPES.contains(&fx.kind.as_str()) {
                errors.push(ValidationError::new(
                    "UNKNOWN_EFFECT_TYPE",
                    format!("{tprefix}.fx[{fi}].type"),
                    format!("unknown effect type: {:?}", fx.kind),
                ));
            }

            // fx params: 認識されない (= レンダリングで無視される) キーを警告。
            // Effect.params は serde(flatten) なので、 JSON 上の path も
            // `tracks[ti].fx[fi].<key>` (params キーを噛ませない)。
            if let Some(known) = effect_param_keys(&fx.kind) {
                for key in fx.params.keys() {
                    if !known.iter().any(|k| k == key) {
                        errors.push(ValidationError::warning(
                            "UNKNOWN_PARAM",
                            format!("{tprefix}.fx[{fi}].{key}"),
                            format!(
                                "param {key:?} is not recognized by effect {:?} and will be ignored at render time (known params: {known:?})",
                                fx.kind
                            ),
                        ));
                    }
                }
            }
        }

        // notes
        for (ni, note) in track.notes.iter().enumerate() {
            let nprefix = format!("{tprefix}.notes[{ni}]");
            if !(note.t.is_finite() && note.t >= 0.0) {
                errors.push(ValidationError::new(
                    "INVALID_NOTE",
                    format!("{nprefix}.t"),
                    format!("t must be a finite non-negative number, got {}", note.t),
                ));
            }
            if !(note.dur.is_finite() && note.dur > 0.0) {
                errors.push(ValidationError::new(
                    "INVALID_NOTE",
                    format!("{nprefix}.dur"),
                    format!("dur must be a finite positive number, got {}", note.dur),
                ));
            }
            // vel は u8 なので 0..=127 の上限のみ要チェック (255 までデシリアライズされうる)
            if note.vel > 127 {
                errors.push(ValidationError::new(
                    "INVALID_NOTE",
                    format!("{nprefix}.vel"),
                    format!("velocity must be 0..=127, got {}", note.vel),
                ));
            }

            // pitch
            if is_drum {
                match note.pitch.as_drum_key() {
                    Ok(k) if KNOWN_DRUM_KEYS.contains(&k) => {}
                    Ok(k) => errors.push(ValidationError::new(
                        "INVALID_NOTE",
                        format!("{nprefix}.pitch"),
                        format!("unknown drum key: {k:?}"),
                    )),
                    Err(_) => errors.push(ValidationError::new(
                        "INVALID_NOTE",
                        format!("{nprefix}.pitch"),
                        "drum_kit track requires a drum key string (e.g. \"kick\")",
                    )),
                }
            } else {
                match &note.pitch {
                    Pitch::Midi(_) => {}
                    Pitch::Name(_) => {
                        if let Err(e) = note.pitch.as_midi() {
                            errors.push(ValidationError::new(
                                "INVALID_NOTE",
                                format!("{nprefix}.pitch"),
                                e.to_string(),
                            ));
                        }
                    }
                }
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Effect, Instrument, Metadata, Note, Pitch, Track};
    use serde_json::Map;

    fn ok_song() -> Song {
        Song {
            version: crate::SCHEMA_VERSION.into(),
            metadata: Metadata {
                name: "x".into(),
                bpm: 120,
                key: None,
                time_signature: [4, 4],
                master_gain: 1.0,
                created_at: None,
                tags: vec![],
            },
            tracks: vec![Track {
                id: "lead".into(),
                name: "Lead".into(),
                instrument: Instrument::new("saw_lead"),
                volume: 0.8,
                pan: 0.0,
                mute: false,
                solo: false,
                fx: vec![Effect {
                    kind: "reverb".into(),
                    params: Map::new(),
                }],
                notes: vec![Note {
                    t: 0.0,
                    pitch: Pitch::Name("C4".into()),
                    dur: 0.5,
                    vel: 100,
                }],
            }],
        }
    }

    #[test]
    fn happy_path() {
        assert!(validate(&ok_song()).is_empty());
    }

    #[test]
    fn rejects_bad_bpm() {
        let mut s = ok_song();
        s.metadata.bpm = 5;
        let errs = validate(&s);
        assert!(errs.iter().any(|e| e.path == "metadata.bpm"));
    }

    #[test]
    fn rejects_bad_master_gain() {
        let mut s = ok_song();
        s.metadata.master_gain = -0.1;
        assert!(validate(&s)
            .iter()
            .any(|e| e.path == "metadata.master_gain"));
        s.metadata.master_gain = 4.5;
        assert!(validate(&s)
            .iter()
            .any(|e| e.path == "metadata.master_gain"));
        s.metadata.master_gain = f32::NAN;
        assert!(validate(&s)
            .iter()
            .any(|e| e.path == "metadata.master_gain"));
        s.metadata.master_gain = 2.5;
        assert!(validate(&s).is_empty());
    }

    #[test]
    fn rejects_duplicate_track_ids() {
        let mut s = ok_song();
        let mut t2 = s.tracks[0].clone();
        t2.notes.clear();
        s.tracks.push(t2);
        let errs = validate(&s);
        assert!(errs.iter().any(|e| e.code == "TRACK_ID_DUPLICATE"));
    }

    #[test]
    fn rejects_unknown_instrument() {
        let mut s = ok_song();
        s.tracks[0].instrument = Instrument::new("xyz");
        let errs = validate(&s);
        assert!(errs.iter().any(|e| e.code == "UNKNOWN_INSTRUMENT_TYPE"));
    }

    #[test]
    fn rejects_unknown_effect() {
        let mut s = ok_song();
        s.tracks[0].fx.push(Effect {
            kind: "warp_drive".into(),
            params: Map::new(),
        });
        let errs = validate(&s);
        assert!(errs.iter().any(|e| e.code == "UNKNOWN_EFFECT_TYPE"));
    }

    #[test]
    fn rejects_bad_note_pitch() {
        let mut s = ok_song();
        s.tracks[0].notes[0].pitch = Pitch::Name("Q4".into());
        let errs = validate(&s);
        assert!(errs
            .iter()
            .any(|e| e.code == "INVALID_NOTE" && e.path.ends_with(".pitch")));
    }

    #[test]
    fn drum_track_requires_drum_key() {
        let mut s = ok_song();
        s.tracks[0].instrument = Instrument::new("drum_kit");
        s.tracks[0].notes[0].pitch = Pitch::Name("C4".into());
        let errs = validate(&s);
        assert!(errs
            .iter()
            .any(|e| e.code == "INVALID_NOTE" && e.path.ends_with(".pitch")));

        // 正しい drum key なら通る
        s.tracks[0].notes[0].pitch = Pitch::Name("kick".into());
        assert!(validate(&s).is_empty());
    }

    #[test]
    fn soundfont_track_requires_file_param() {
        let mut s = ok_song();
        s.tracks[0].instrument = Instrument::new("soundfont");
        let errs = validate(&s);
        assert!(
            errs.iter()
                .any(|e| e.code == "INVALID_SCHEMA" && e.path.ends_with(".instrument.params")),
            "expected missing-file error, got: {errs:?}"
        );
    }

    #[test]
    fn soundfont_track_reports_missing_file() {
        let mut s = ok_song();
        let mut inst = Instrument::new("soundfont");
        inst.params.insert(
            "file".into(),
            serde_json::json!("/nonexistent/codetta-test/abs.sf2"),
        );
        s.tracks[0].instrument = inst;
        let errs = validate(&s);
        assert!(
            errs.iter().any(|e| e.code == "SOUNDFONT_FILE_NOT_FOUND"),
            "expected SOUNDFONT_FILE_NOT_FOUND, got: {errs:?}"
        );
    }

    #[test]
    fn rejects_negative_time_and_zero_duration() {
        let mut s = ok_song();
        s.tracks[0].notes[0].t = -1.0;
        s.tracks[0].notes[0].dur = 0.0;
        let errs = validate(&s);
        assert!(errs.iter().any(|e| e.path.ends_with(".t")));
        assert!(errs.iter().any(|e| e.path.ends_with(".dur")));
    }

    #[test]
    fn warns_on_unknown_param_for_saw() {
        // saw は ADSR のみ受け取る。 pulse_width は square 系の param なので warn
        let mut s = ok_song();
        s.tracks[0].instrument = Instrument::new("saw");
        s.tracks[0]
            .instrument
            .params
            .insert("pulse_width".into(), serde_json::json!(0.3));
        let errs = validate(&s);
        let warn = errs.iter().find(|e| e.code == "UNKNOWN_PARAM");
        assert!(
            warn.is_some(),
            "expected UNKNOWN_PARAM warning, got: {errs:?}"
        );
        let w = warn.unwrap();
        assert!(
            w.is_warning(),
            "expected severity=warning, got: {:?}",
            w.severity
        );
        assert!(w.path.ends_with(".instrument.params.pulse_width"));
        // error 級は出ない
        assert!(
            !errs.iter().any(|e| e.is_error()),
            "unexpected errors: {errs:?}"
        );
    }

    #[test]
    fn warns_on_unknown_param_for_square_bass() {
        // square_bass は ADSR + pulse_width のみ。 detune_cents は saw_pad 用
        let mut s = ok_song();
        s.tracks[0].instrument = Instrument::new("square_bass");
        s.tracks[0]
            .instrument
            .params
            .insert("detune_cents".into(), serde_json::json!(10.0));
        let errs = validate(&s);
        assert!(
            errs.iter().any(|e| e.code == "UNKNOWN_PARAM"
                && e.is_warning()
                && e.path.ends_with(".instrument.params.detune_cents")),
            "expected UNKNOWN_PARAM warning for detune_cents, got: {errs:?}"
        );
    }

    #[test]
    fn known_params_do_not_warn() {
        // square に pulse_width は OK
        let mut s = ok_song();
        s.tracks[0].instrument = Instrument::new("square");
        s.tracks[0]
            .instrument
            .params
            .insert("pulse_width".into(), serde_json::json!(0.3));
        s.tracks[0]
            .instrument
            .params
            .insert("attack".into(), serde_json::json!(0.02));
        let errs = validate(&s);
        assert!(
            errs.is_empty(),
            "expected no errors/warnings, got: {errs:?}"
        );
    }

    #[test]
    fn warns_on_unknown_param_for_reverb() {
        // reverb は size/damp/mix のみ。 feedback は delay 用 param なので warn
        let mut s = ok_song();
        s.tracks[0].fx[0]
            .params
            .insert("feedback".into(), serde_json::json!(0.5));
        let errs = validate(&s);
        let warn = errs.iter().find(|e| e.code == "UNKNOWN_PARAM");
        assert!(
            warn.is_some(),
            "expected UNKNOWN_PARAM warning, got: {errs:?}"
        );
        let w = warn.unwrap();
        assert!(
            w.is_warning(),
            "expected severity=warning, got: {:?}",
            w.severity
        );
        assert!(w.path.ends_with(".fx[0].feedback"));
        assert!(
            !errs.iter().any(|e| e.is_error()),
            "unexpected errors: {errs:?}"
        );
    }

    #[test]
    fn warns_on_unknown_param_for_lowpass() {
        // lowpass は cutoff/q のみ。 mix は reverb/delay 用 param なので warn
        let mut s = ok_song();
        s.tracks[0].fx[0] = Effect {
            kind: "lowpass".into(),
            params: {
                let mut m = Map::new();
                m.insert("cutoff".into(), serde_json::json!(800.0));
                m.insert("mix".into(), serde_json::json!(0.5));
                m
            },
        };
        let errs = validate(&s);
        assert!(
            errs.iter().any(|e| e.code == "UNKNOWN_PARAM"
                && e.is_warning()
                && e.path.ends_with(".fx[0].mix")),
            "expected UNKNOWN_PARAM warning for mix, got: {errs:?}"
        );
    }

    #[test]
    fn known_fx_params_do_not_warn() {
        // delay の time/feedback/mix は OK
        let mut s = ok_song();
        s.tracks[0].fx[0] = Effect {
            kind: "delay".into(),
            params: {
                let mut m = Map::new();
                m.insert("time".into(), serde_json::json!("1/8"));
                m.insert("feedback".into(), serde_json::json!(0.4));
                m.insert("mix".into(), serde_json::json!(0.3));
                m
            },
        };
        let errs = validate(&s);
        assert!(
            errs.is_empty(),
            "expected no errors/warnings, got: {errs:?}"
        );
    }

    #[test]
    fn unknown_effect_kind_skips_param_warning() {
        // effect type 自体が未知なら UNKNOWN_EFFECT_TYPE のみ。 param までは追わない
        let mut s = ok_song();
        s.tracks[0].fx[0] = Effect {
            kind: "warp_drive".into(),
            params: {
                let mut m = Map::new();
                m.insert("foo".into(), serde_json::json!(1.0));
                m
            },
        };
        let errs = validate(&s);
        assert!(errs.iter().any(|e| e.code == "UNKNOWN_EFFECT_TYPE"));
        assert!(
            !errs.iter().any(|e| e.code == "UNKNOWN_PARAM"),
            "should not warn on params when effect type itself is unknown: {errs:?}"
        );
    }

    #[test]
    fn unknown_instrument_kind_skips_param_warning() {
        // 楽器 type 自体が未知なら UNKNOWN_INSTRUMENT_TYPE のみ。 param までは追わない
        let mut s = ok_song();
        s.tracks[0].instrument = Instrument::new("nonexistent");
        s.tracks[0]
            .instrument
            .params
            .insert("foo".into(), serde_json::json!(1.0));
        let errs = validate(&s);
        assert!(errs.iter().any(|e| e.code == "UNKNOWN_INSTRUMENT_TYPE"));
        assert!(
            !errs.iter().any(|e| e.code == "UNKNOWN_PARAM"),
            "should not warn on params when instrument type itself is unknown: {errs:?}"
        );
    }
}
