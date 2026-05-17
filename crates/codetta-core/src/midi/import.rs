//! MIDI (.mid) → `.codetta` Song 変換。
//!
//! ADR: docs/design/08-midi.md 「MIDI import の信号フロー」「channel ↔ track マッピング」
//!
//! 主な決定:
//! - SMF Type 0 / Type 1 両対応。 Type 2 (Sequential) は ADR スコープ外で reject。
//! - PPQ ベース timing 必須 (SMPTE timecode は reject)。 tick → beat は `beat = tick / ppq`。
//! - channel → track のマッピングは A 案 (1 ch = 1 track) で、 並びは channel index 昇順
//!   (drum ch10 = idx 9 はその位置に挟まる)。 ADR L85 が「first note on tick 順」と書いて
//!   いた箇所は、 text-meta の `tracks[]` 並び (= channel index 昇順) との整合を取るため
//!   本実装で確定させる (= ADR と合わせて更新済)。
//! - ch10 (= MIDI channel idx 9) は `bank=128` drum track として 1 つ。 ノート pitch は
//!   `Pitch::Midi(n)` の数値固定 (= ADR L118)。 要素名キー逆変換は CDT-3 では実装しない。
//! - 拡張属性 (master_gain / fx / SF2 preset 詳細) は ExtensionsMode に従って復元:
//!   text-meta (default) → sidecar fallback → MIDI のみ。

use std::path::Path;

use midly::{Format, MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::midi::error::MidiError;
use crate::midi::extensions::{
    load_sidecar, parse_text_meta, sidecar_path_for, ExtensionsMode, ExtensionsPayload,
    ExtensionsRoot,
};
use crate::migrate::DEFAULT_SF2;
use crate::model::{Effect, Instrument, Metadata, Note, Pitch, Song, Track};
use crate::synth::soundfont::{list_soundfont_presets, resolve_soundfont_path};
use crate::SCHEMA_VERSION;

/// MIDI ch10 (1-origin) = MIDI channel index 9 (0-origin) は GM/GS で drum bank 固定。
const DRUM_CHANNEL_INDEX: u8 = 9;
/// MIDI 仕様で program change が無い時の暗黙 default (= GM Program 0)。
const DEFAULT_BPM: u32 = 120;
const DEFAULT_VOLUME: f32 = 0.8;

#[derive(Debug, Clone)]
pub struct MidiImportOptions {
    /// 拡張属性の取り出しモード (default: `TextMeta`)。
    pub extensions: ExtensionsMode,
    /// SF2 ファイル名 (= `Instrument.params.file` に書く値 + preset 存在確認の根拠)。
    /// 省略時は [`DEFAULT_SF2`] を `file` に書くが、 preset 存在確認は行わない。
    pub sf2_file: Option<String>,
    /// 生成される Song の `metadata.name`。 省略時は MIDI path の stem を使う。
    pub song_name: Option<String>,
}

impl Default for MidiImportOptions {
    fn default() -> Self {
        Self {
            extensions: ExtensionsMode::TextMeta,
            sf2_file: None,
            song_name: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MidiImportOutcome {
    pub song: Song,
    pub warnings: Vec<MidiImportWarning>,
    pub extensions_recovered: ExtensionsRecovered,
    /// "Type 0" / "Type 1" — telemetry 用。
    pub source_format: String,
    pub ppq: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct MidiImportWarning {
    pub code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionsRecovered {
    pub master_gain: bool,
    pub fx: bool,
    pub soundfont_params: bool,
    /// "text-meta" / "sidecar" / "none"
    pub source: &'static str,
}

pub fn import_midi(
    path: impl AsRef<Path>,
    options: &MidiImportOptions,
) -> Result<MidiImportOutcome, MidiError> {
    let path = path.as_ref();

    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(MidiError::FileNotFound(path.to_path_buf()));
        }
        Err(e) => return Err(MidiError::Io(e)),
    };
    let smf = Smf::parse(&bytes).map_err(|e| MidiError::Parse(format!("{e:?}")))?;

    if matches!(smf.header.format, Format::Sequential) {
        return Err(MidiError::UnsupportedFormat(
            "Type 2 (Sequential) is out of scope".into(),
        ));
    }
    let ppq: u16 = match smf.header.timing {
        Timing::Metrical(n) => n.as_int(),
        Timing::Timecode(_, _) => return Err(MidiError::UnsupportedTiming),
    };
    let source_format = match smf.header.format {
        Format::SingleTrack => "Type 0",
        Format::Parallel => "Type 1",
        Format::Sequential => unreachable!(),
    };

    let mut warnings: Vec<MidiImportWarning> = Vec::new();
    let mut text_meta_payload: Option<Result<ExtensionsPayload, MidiError>> = None;
    let mut tempo_uspq: Option<u32> = None;
    let mut time_signature: Option<[u32; 2]> = None;

    let mut channels: Vec<ChannelState> = (0..16).map(|_| ChannelState::default()).collect();

    for (track_idx, track) in smf.tracks.iter().enumerate() {
        let mut abs_tick: u64 = 0;
        let mut first_text_meta_seen = false;
        for ev in track {
            abs_tick = abs_tick.saturating_add(u64::from(ev.delta.as_int()));
            match ev.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(uspq))
                    if track_idx == 0 && tempo_uspq.is_none() =>
                {
                    tempo_uspq = Some(uspq.as_int());
                }
                TrackEventKind::Meta(MetaMessage::TimeSignature(num, denom_pow2, _, _))
                    if track_idx == 0 && time_signature.is_none() =>
                {
                    let denom = 1u32 << u32::from(denom_pow2);
                    time_signature = Some([u32::from(num), denom]);
                }
                TrackEventKind::Meta(MetaMessage::Text(bytes))
                    if track_idx == 0 && !first_text_meta_seen =>
                {
                    if let Some(parsed) = parse_text_meta(bytes) {
                        first_text_meta_seen = true;
                        text_meta_payload = Some(parsed);
                    }
                }
                TrackEventKind::Midi { channel, message } => {
                    let ch_idx = channel.as_int() as usize;
                    let st = &mut channels[ch_idx];
                    match message {
                        MidiMessage::NoteOn { key, vel } => {
                            let k = key.as_int();
                            let v = vel.as_int();
                            if v > 0 {
                                if st.first_note_tick.is_none() {
                                    st.first_note_tick = Some(abs_tick);
                                }
                                st.pending_notes[k as usize] = Some(PendingNoteOn {
                                    start_tick: abs_tick,
                                    velocity: v,
                                });
                            } else if let Some(p) = st.pending_notes[k as usize].take() {
                                st.notes.push(complete_note(p, k, abs_tick));
                            }
                        }
                        MidiMessage::NoteOff { key, .. } => {
                            let k = key.as_int();
                            if let Some(p) = st.pending_notes[k as usize].take() {
                                st.notes.push(complete_note(p, k, abs_tick));
                            }
                        }
                        MidiMessage::ProgramChange { program } => {
                            st.program = Some(program.as_int());
                        }
                        MidiMessage::Controller { controller, value } => {
                            let c = controller.as_int();
                            let v = value.as_int();
                            if c == 7 {
                                st.volume = Some(v);
                            } else if c == 10 {
                                st.pan = Some(v);
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    let text_meta = match text_meta_payload {
        None => None,
        Some(Ok(p)) => Some(p),
        Some(Err(e)) => {
            warnings.push(MidiImportWarning {
                code: "EXTENSIONS_INVALID",
                channel: None,
                message: format!(
                    "text-meta JSON is invalid: {e}; falling back to MIDI-only restoration"
                ),
            });
            None
        }
    };

    let mut ext_source: &'static str = "none";
    let extensions: Option<ExtensionsPayload> = match options.extensions {
        ExtensionsMode::None => None,
        ExtensionsMode::TextMeta => match text_meta {
            Some(p) => {
                ext_source = "text-meta";
                Some(p)
            }
            None => match load_sidecar(&sidecar_path_for(path)) {
                Ok(Some(p)) => {
                    ext_source = "sidecar";
                    Some(p)
                }
                Ok(None) => None,
                Err(e) => {
                    warnings.push(MidiImportWarning {
                        code: "EXTENSIONS_INVALID",
                        channel: None,
                        message: format!(
                            "sidecar JSON is invalid: {e}; falling back to MIDI-only restoration"
                        ),
                    });
                    None
                }
            },
        },
        ExtensionsMode::Sidecar => match load_sidecar(&sidecar_path_for(path)) {
            Ok(Some(p)) => {
                ext_source = "sidecar";
                Some(p)
            }
            Ok(None) => None,
            Err(e) => {
                warnings.push(MidiImportWarning {
                    code: "EXTENSIONS_INVALID",
                    channel: None,
                    message: format!(
                        "sidecar JSON is invalid: {e}; falling back to MIDI-only restoration"
                    ),
                });
                None
            }
        },
    };

    let bpm = tempo_uspq
        .map(|us| (60_000_000.0 / us as f32).round() as u32)
        .unwrap_or(DEFAULT_BPM)
        .clamp(20, 300);
    let time_sig = time_signature.unwrap_or([4, 4]);

    let mut metadata = Metadata {
        name: options
            .song_name
            .clone()
            .or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "imported".to_string()),
        bpm,
        key: None,
        time_signature: time_sig,
        master_gain: 1.0,
        created_at: None,
        tags: Vec::new(),
    };

    let mut ext_recovered = ExtensionsRecovered {
        master_gain: false,
        fx: false,
        soundfont_params: false,
        source: ext_source,
    };

    if let Some(ext) = &extensions {
        if let Some(em) = &ext.codetta.metadata {
            if let Some(mg) = em.master_gain {
                metadata.master_gain = mg;
                ext_recovered.master_gain = true;
            }
            if let Some(k) = &em.key {
                metadata.key = Some(k.clone());
            }
            if !em.tags.is_empty() {
                metadata.tags = em.tags.clone();
            }
        }
    }

    let sf2_file_label = options
        .sf2_file
        .clone()
        .unwrap_or_else(|| DEFAULT_SF2.to_string());

    let sf2_preset_set: Option<std::collections::HashSet<(u16, u16)>> = match options
        .sf2_file
        .as_deref()
    {
        None => None,
        Some(f) => {
            let resolved = resolve_soundfont_path(f);
            match list_soundfont_presets(&resolved) {
                Ok((_, presets)) => Some(presets.iter().map(|p| (p.bank, p.preset)).collect()),
                Err(_) => {
                    warnings.push(MidiImportWarning {
                        code: "SOUNDFONT_UNAVAILABLE",
                        channel: None,
                        message: format!(
                            "SF2 file {f:?} could not be loaded; skipping preset existence check"
                        ),
                    });
                    None
                }
            }
        }
    };

    let mut tracks: Vec<Track> = Vec::new();
    for (idx, st) in channels.iter().enumerate() {
        if st.notes.is_empty() && st.program.is_none() {
            continue;
        }

        let ch_idx_u8 = idx as u8;
        let is_drum = ch_idx_u8 == DRUM_CHANNEL_INDEX;
        let raw_preset = u16::from(st.program.unwrap_or(0));
        let bank: u16 = if is_drum { 128 } else { 0 };

        let final_preset = if let Some(set) = &sf2_preset_set {
            if set.contains(&(bank, raw_preset)) {
                raw_preset
            } else {
                warnings.push(MidiImportWarning {
                    code: "PRESET_NOT_FOUND",
                    channel: Some(ch_idx_u8 + 1),
                    message: format!(
                        "SF2 has no preset for (bank={bank}, preset={raw_preset}) on channel {}; \
                         falling back to preset 0",
                        ch_idx_u8 + 1
                    ),
                });
                0
            }
        } else {
            raw_preset
        };

        let mut inst_params = Map::new();
        inst_params.insert("file".into(), Value::String(sf2_file_label.clone()));
        inst_params.insert(
            "preset".into(),
            Value::Number(u64::from(final_preset).into()),
        );
        inst_params.insert("bank".into(), Value::Number(u64::from(bank).into()));

        let id = if is_drum {
            "drums".to_string()
        } else {
            format!("channel-{}", ch_idx_u8 + 1)
        };
        let name = if is_drum {
            "Drums".to_string()
        } else {
            format!("Channel {}", ch_idx_u8 + 1)
        };

        let volume = st
            .volume
            .map(|v| f32::from(v) / 127.0)
            .unwrap_or(DEFAULT_VOLUME);
        let pan = st
            .pan
            .map(|v| ((f32::from(v) - 64.0) / 63.0).clamp(-1.0, 1.0))
            .unwrap_or(0.0);

        let mut notes: Vec<Note> = st
            .notes
            .iter()
            .map(|n| Note {
                t: n.start_tick as f32 / f32::from(ppq),
                pitch: Pitch::Midi(n.midi_key),
                dur: ((n.end_tick.saturating_sub(n.start_tick)) as f32 / f32::from(ppq)).max(1e-6),
                vel: n.velocity,
            })
            .collect();
        notes.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));

        tracks.push(Track {
            id,
            name,
            instrument: Instrument {
                kind: "soundfont".to_string(),
                params: inst_params,
            },
            volume,
            pan,
            mute: false,
            solo: false,
            fx: vec![],
            notes,
        });
    }

    if let Some(ext) = &extensions {
        apply_extensions_to_tracks(&mut tracks, &ext.codetta, &mut ext_recovered, &mut warnings);
    }

    let song = Song {
        version: SCHEMA_VERSION.to_string(),
        metadata,
        tracks,
    };

    Ok(MidiImportOutcome {
        song,
        warnings,
        extensions_recovered: ext_recovered,
        source_format: source_format.to_string(),
        ppq,
    })
}

fn apply_extensions_to_tracks(
    tracks: &mut [Track],
    root: &ExtensionsRoot,
    recovered: &mut ExtensionsRecovered,
    warnings: &mut Vec<MidiImportWarning>,
) {
    if root.tracks.is_empty() {
        return;
    }
    if root.tracks.len() != tracks.len() {
        warnings.push(MidiImportWarning {
            code: "EXTENSIONS_TRACK_COUNT_MISMATCH",
            channel: None,
            message: format!(
                "text-meta tracks[] has {} entries but MIDI produced {} tracks; \
                 applying overrides positionally up to the shorter length",
                root.tracks.len(),
                tracks.len()
            ),
        });
    }
    for (track, ext_tr) in tracks.iter_mut().zip(root.tracks.iter()) {
        if let Some(id) = &ext_tr.id {
            track.id = id.clone();
        }
        if let Some(name) = &ext_tr.name {
            track.name = name.clone();
        }
        if let Some(inst_val) = &ext_tr.instrument {
            match serde_json::from_value::<Instrument>(inst_val.clone()) {
                Ok(inst) => {
                    track.instrument = inst;
                    recovered.soundfont_params = true;
                }
                Err(_) => warnings.push(MidiImportWarning {
                    code: "EXTENSIONS_INVALID",
                    channel: None,
                    message: format!(
                        "extension track {:?} has unparseable instrument; keeping MIDI-derived value",
                        track.id
                    ),
                }),
            }
        }
        if let Some(fx_val) = &ext_tr.fx {
            match serde_json::from_value::<Vec<Effect>>(fx_val.clone()) {
                Ok(fx) => {
                    if !fx.is_empty() {
                        recovered.fx = true;
                    }
                    track.fx = fx;
                }
                Err(_) => warnings.push(MidiImportWarning {
                    code: "EXTENSIONS_INVALID",
                    channel: None,
                    message: format!(
                        "extension track {:?} has unparseable fx; keeping MIDI-derived value",
                        track.id
                    ),
                }),
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ChannelState {
    first_note_tick: Option<u64>,
    pending_notes: [Option<PendingNoteOn>; 128],
    notes: Vec<CompletedNote>,
    program: Option<u8>,
    volume: Option<u8>,
    pan: Option<u8>,
}

impl Default for ChannelState {
    fn default() -> Self {
        const INIT: Option<PendingNoteOn> = None;
        Self {
            first_note_tick: None,
            pending_notes: [INIT; 128],
            notes: Vec::new(),
            program: None,
            volume: None,
            pan: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PendingNoteOn {
    start_tick: u64,
    velocity: u8,
}

#[derive(Debug, Clone, Copy)]
struct CompletedNote {
    start_tick: u64,
    end_tick: u64,
    midi_key: u8,
    velocity: u8,
}

fn complete_note(pending: PendingNoteOn, key: u8, end_tick: u64) -> CompletedNote {
    CompletedNote {
        start_tick: pending.start_tick,
        end_tick: end_tick.max(pending.start_tick + 1),
        midi_key: key,
        velocity: pending.velocity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use midly::{
        num::{u15, u24, u4, u7},
        Header, MetaMessage, MidiMessage, Track as MidlyTrack, TrackEvent,
    };

    fn tempdir() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("codetta-midi-{nanos}-{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_smf(tracks: Vec<MidlyTrack<'_>>, ppq: u16) -> std::path::PathBuf {
        let smf = Smf {
            header: Header::new(Format::Parallel, Timing::Metrical(u15::new(ppq))),
            tracks,
        };
        let dir = tempdir();
        let path = dir.join("song.mid");
        let mut buf: Vec<u8> = Vec::new();
        smf.write(&mut buf).unwrap();
        std::fs::write(&path, buf).unwrap();
        path
    }

    fn ev_delta(delta: u32, kind: TrackEventKind<'_>) -> TrackEvent<'_> {
        use midly::num::u28;
        TrackEvent {
            delta: u28::new(delta),
            kind,
        }
    }

    fn note_on(key: u8, vel: u8) -> MidiMessage {
        MidiMessage::NoteOn {
            key: u7::new(key),
            vel: u7::new(vel),
        }
    }

    fn note_off(key: u8) -> MidiMessage {
        MidiMessage::NoteOff {
            key: u7::new(key),
            vel: u7::new(0x40),
        }
    }

    /// 標準 GM (Type 1): tempo + time sig + 1 melodic track + 1 drum track。
    fn minimal_gm_song(ppq: u16) -> std::path::PathBuf {
        // meta track (= MTrk 0): tempo 120 BPM (= 500_000 usec/quarter) + 4/4
        let meta = vec![
            ev_delta(
                0,
                TrackEventKind::Meta(MetaMessage::Tempo(u24::new(500_000))),
            ),
            ev_delta(
                0,
                TrackEventKind::Meta(MetaMessage::TimeSignature(4, 2, 24, 8)),
            ),
            ev_delta(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ];

        // melodic ch1 (idx 0): program 81 (saw lead), one C4 note (quarter)
        let mel = vec![
            ev_delta(
                0,
                TrackEventKind::Midi {
                    channel: u4::new(0),
                    message: MidiMessage::ProgramChange {
                        program: u7::new(81),
                    },
                },
            ),
            ev_delta(
                0,
                TrackEventKind::Midi {
                    channel: u4::new(0),
                    message: note_on(60, 100),
                },
            ),
            ev_delta(
                u32::from(ppq),
                TrackEventKind::Midi {
                    channel: u4::new(0),
                    message: note_off(60),
                },
            ),
            ev_delta(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ];

        // drum ch10 (idx 9): kick (36) at t=0, no program change
        let drum = vec![
            ev_delta(
                0,
                TrackEventKind::Midi {
                    channel: u4::new(9),
                    message: note_on(36, 110),
                },
            ),
            ev_delta(
                u32::from(ppq / 2),
                TrackEventKind::Midi {
                    channel: u4::new(9),
                    message: note_off(36),
                },
            ),
            ev_delta(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ];

        write_smf(vec![meta, mel, drum], ppq)
    }

    #[test]
    fn imports_minimal_gm_song_with_channel_mapping() {
        let path = minimal_gm_song(480);
        let outcome = import_midi(&path, &MidiImportOptions::default()).expect("import");

        assert_eq!(outcome.source_format, "Type 1");
        assert_eq!(outcome.ppq, 480);
        assert_eq!(outcome.song.metadata.bpm, 120);
        assert_eq!(outcome.song.metadata.time_signature, [4, 4]);

        assert_eq!(outcome.song.tracks.len(), 2);

        let mel = &outcome.song.tracks[0];
        assert_eq!(mel.id, "channel-1");
        assert_eq!(mel.instrument.kind, "soundfont");
        assert_eq!(mel.instrument.params["preset"], 81);
        assert_eq!(mel.instrument.params["bank"], 0);
        assert_eq!(mel.notes.len(), 1);
        assert_eq!(mel.notes[0].pitch, Pitch::Midi(60));
        assert!((mel.notes[0].t - 0.0).abs() < 1e-6);
        assert!((mel.notes[0].dur - 1.0).abs() < 1e-3);

        let drum = &outcome.song.tracks[1];
        assert_eq!(drum.id, "drums");
        assert_eq!(drum.instrument.params["bank"], 128);
        assert_eq!(drum.instrument.params["preset"], 0);
        assert_eq!(drum.notes[0].pitch, Pitch::Midi(36));

        assert_eq!(outcome.extensions_recovered.source, "none");
        assert!(!outcome.extensions_recovered.master_gain);
    }

    #[test]
    fn text_meta_overrides_track_metadata() {
        let ppq: u16 = 480;
        let text_meta = br#"{"codetta":{"version":"0.2","metadata":{"master_gain":2.5,"key":"Am","tags":["test"]},"tracks":[{"id":"lead","name":"Saw Lead","instrument":{"type":"soundfont","params":{"file":"custom.sf2","preset":81,"bank":0}},"fx":[{"type":"reverb","mix":0.2}]}]}}"#;
        let meta = vec![
            ev_delta(
                0,
                TrackEventKind::Meta(MetaMessage::Text(text_meta.as_slice())),
            ),
            ev_delta(
                0,
                TrackEventKind::Meta(MetaMessage::Tempo(u24::new(500_000))),
            ),
            ev_delta(
                0,
                TrackEventKind::Meta(MetaMessage::TimeSignature(4, 2, 24, 8)),
            ),
            ev_delta(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ];
        let mel = vec![
            ev_delta(
                0,
                TrackEventKind::Midi {
                    channel: u4::new(0),
                    message: note_on(60, 100),
                },
            ),
            ev_delta(
                u32::from(ppq),
                TrackEventKind::Midi {
                    channel: u4::new(0),
                    message: note_off(60),
                },
            ),
            ev_delta(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ];
        let path = write_smf(vec![meta, mel], ppq);

        let outcome = import_midi(&path, &MidiImportOptions::default()).expect("import");
        assert_eq!(outcome.extensions_recovered.source, "text-meta");
        assert!(outcome.extensions_recovered.master_gain);
        assert!(outcome.extensions_recovered.fx);
        assert!(outcome.extensions_recovered.soundfont_params);
        assert!((outcome.song.metadata.master_gain - 2.5).abs() < 1e-6);
        assert_eq!(outcome.song.metadata.key.as_deref(), Some("Am"));
        assert_eq!(outcome.song.metadata.tags, vec!["test".to_string()]);

        let track = &outcome.song.tracks[0];
        assert_eq!(track.id, "lead");
        assert_eq!(track.name, "Saw Lead");
        assert_eq!(track.instrument.params["file"], "custom.sf2");
        assert_eq!(track.fx.len(), 1);
        assert_eq!(track.fx[0].kind, "reverb");
    }

    #[test]
    fn unsupported_smpte_timing_is_rejected() {
        use midly::Header;
        let smf = Smf {
            header: Header::new(Format::Parallel, Timing::Timecode(midly::Fps::Fps25, 40)),
            tracks: vec![vec![ev_delta(
                0,
                TrackEventKind::Meta(MetaMessage::EndOfTrack),
            )]],
        };
        let dir = tempdir();
        let path = dir.join("smpte.mid");
        let mut buf = Vec::new();
        smf.write(&mut buf).unwrap();
        std::fs::write(&path, buf).unwrap();

        let err = import_midi(&path, &MidiImportOptions::default()).unwrap_err();
        assert!(matches!(err, MidiError::UnsupportedTiming));
    }

    #[test]
    fn file_not_found_returns_file_not_found_error() {
        let err = import_midi(
            "/nonexistent/codetta-midi-import/missing.mid",
            &MidiImportOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, MidiError::FileNotFound(_)));
    }

    #[test]
    fn parses_type0_single_track() {
        let ppq: u16 = 480;
        // Type 0 = 全 channel が 1 MTrk に混在
        let combined = vec![
            ev_delta(
                0,
                TrackEventKind::Meta(MetaMessage::Tempo(u24::new(500_000))),
            ),
            ev_delta(
                0,
                TrackEventKind::Midi {
                    channel: u4::new(0),
                    message: note_on(60, 100),
                },
            ),
            ev_delta(
                u32::from(ppq),
                TrackEventKind::Midi {
                    channel: u4::new(0),
                    message: note_off(60),
                },
            ),
            ev_delta(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ];
        let smf = Smf {
            header: Header::new(Format::SingleTrack, Timing::Metrical(u15::new(ppq))),
            tracks: vec![combined],
        };
        let dir = tempdir();
        let path = dir.join("type0.mid");
        let mut buf = Vec::new();
        smf.write(&mut buf).unwrap();
        std::fs::write(&path, buf).unwrap();

        let outcome = import_midi(&path, &MidiImportOptions::default()).expect("import");
        assert_eq!(outcome.source_format, "Type 0");
        assert_eq!(outcome.song.tracks.len(), 1);
        assert_eq!(outcome.song.tracks[0].notes.len(), 1);
    }
}
