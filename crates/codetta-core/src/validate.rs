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
    "kick", "snare", "hh_closed", "hh_open", "clap", "crash", "ride", "tom_lo", "tom_mid",
    "tom_hi",
];

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

        // effects
        for (fi, fx) in track.fx.iter().enumerate() {
            if !KNOWN_EFFECT_TYPES.contains(&fx.kind.as_str()) {
                errors.push(ValidationError::new(
                    "UNKNOWN_EFFECT_TYPE",
                    format!("{tprefix}.fx[{fi}].type"),
                    format!("unknown effect type: {:?}", fx.kind),
                ));
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
        assert!(errs.iter().any(|e| e.code == "INVALID_NOTE" && e.path.ends_with(".pitch")));
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
        assert!(errs.iter().any(|e|
            e.code == "INVALID_SCHEMA" && e.path.ends_with(".instrument.params")
        ), "expected missing-file error, got: {errs:?}");
    }

    #[test]
    fn soundfont_track_reports_missing_file() {
        let mut s = ok_song();
        let mut inst = Instrument::new("soundfont");
        inst.params.insert("file".into(), serde_json::json!("/nonexistent/codetta-test/abs.sf2"));
        s.tracks[0].instrument = inst;
        let errs = validate(&s);
        assert!(errs.iter().any(|e| e.code == "SOUNDFONT_FILE_NOT_FOUND"),
            "expected SOUNDFONT_FILE_NOT_FOUND, got: {errs:?}");
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
}
