//! `.codetta` Song → 標準 MIDI (.mid) 変換。
//!
//! ADR: docs/design/08-midi.md 「MIDI export の信号フロー」「channel ↔ track マッピング」
//!
//! 主な決定:
//! - 出力は SMF Type 1 (multi-track) 固定。 Type 0 は import のみサポート。
//! - PPQ は options で指定 (default 480、 ADR L43)。
//! - channel ↔ track は A 案 (= 1 track = 1 channel)、 drum (bank=128) は ch10 (idx 9) 専用。
//!   melodic は ch1, ch2, ..., ch9, ch11, ..., ch16 の順に出現順で割当。
//! - drum track の Pitch::Name は **要素名キー (= "kick" / "snare" / ...) を優先**、
//!   見つからなければ ノート名 (= "C4" 等) として解釈する (ADR L107-L108)。
//!   Pitch::Midi(n) はそのまま。
//! - 内蔵 synth (0.1 legacy) は本関数では受け付けない (= soundfont 一本固定)。
//!   CLI / MCP layer で in-memory migrate (0.1 → 0.2) を済ませてから渡す前提。
//! - 拡張属性 (master_gain / fx / SF2 preset 詳細) は `extensions` モードに従って書き出す:
//!   text-meta (default) → MTrk 0 先頭 Text Meta、 sidecar → 別ファイル、 none → 書かない。
//! - tempo / time signature は MTrk 0 先頭で 1 つだけ書く (= 曲全体固定、 ADR L44)。

use std::path::{Path, PathBuf};

use midly::{
    num::{u15, u24, u28, u4, u7},
    Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind,
};
use serde::Serialize;

use crate::midi::error::MidiError;
use crate::midi::extensions::{
    build_text_meta_payload, payload_to_text_meta_bytes, save_sidecar, sidecar_path_for,
    track_is_drum, tracks_in_channel_order, ExtensionsMode,
};
use crate::model::{parse_note_name, Note, Pitch, Song, Track};
use crate::synth::soundfont::{drum_key_to_midi, SoundFontParams};
use crate::SCHEMA_VERSION;

/// MIDI ch10 (1-origin) = MIDI channel index 9 (0-origin)。 drum bank 固定 channel。
const DRUM_CHANNEL_INDEX: u8 = 9;
/// melodic 用 channel の最大 index (0-origin、 ch16 まで)。 ADR L75: 15 melodic + 1 drum (ch10)。
const MAX_MELODIC_CHANNELS: usize = 15;
/// PPQ default (ADR L43)。
pub const DEFAULT_PPQ: u16 = 480;
/// note off velocity 固定 (ADR L47)。
const NOTE_OFF_VELOCITY: u8 = 0x40;
/// MIDI Channel Volume CC 番号 (= CC7、 0-127)。
const CC_VOLUME: u8 = 7;
/// MIDI Channel Pan CC 番号 (= CC10、 0-127、 64 が中央)。
const CC_PAN: u8 = 10;

#[derive(Debug, Clone)]
pub struct MidiExportOptions {
    /// 拡張属性の書き出しモード (default: `TextMeta`)。
    pub extensions: ExtensionsMode,
    /// PPQ (= ticks per quarter)。 default 480 (ADR L43)。
    pub ppq: u16,
}

impl Default for MidiExportOptions {
    fn default() -> Self {
        Self {
            extensions: ExtensionsMode::TextMeta,
            ppq: DEFAULT_PPQ,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MidiExportOutcome {
    /// 常に "Type 1"。 telemetry / CLI 出力用。
    pub format: &'static str,
    pub ppq: u16,
    pub track_count: usize,
    /// `extensions = TextMeta` で MTrk 0 に JSON を書いたか。
    pub text_meta_written: bool,
    /// `extensions = Sidecar` で sidecar JSON を出した path。 書いていなければ `None`。
    pub sidecar_written: Option<PathBuf>,
    pub warnings: Vec<MidiExportWarning>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MidiExportWarning {
    pub code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_id: Option<String>,
    pub message: String,
}

/// Song を `.mid` ファイル (SMF Type 1) として書き出す。
///
/// 入力 Song は schema 0.2 (= `soundfont` 一本) を前提とする。 0.1 入力は呼び出し側
/// (CLI / MCP) で in-memory migrate を済ませてから渡す。
pub fn export_song(
    song: &Song,
    path: impl AsRef<Path>,
    options: &MidiExportOptions,
) -> Result<MidiExportOutcome, MidiError> {
    let path = path.as_ref();

    if song.version != SCHEMA_VERSION {
        return Err(MidiError::UnsupportedFormat(format!(
            "song version is {:?}; export expects schema {} (run migrate first)",
            song.version, SCHEMA_VERSION
        )));
    }

    let ppq = options.ppq.max(1);

    let assignments = assign_channels(song)?;

    let mut warnings: Vec<MidiExportWarning> = Vec::new();
    let mut text_meta_written = false;
    let mut sidecar_written: Option<PathBuf> = None;

    let text_meta_bytes: Option<Vec<u8>> = match options.extensions {
        ExtensionsMode::TextMeta => {
            let payload = build_text_meta_payload(song);
            let bytes = payload_to_text_meta_bytes(&payload);
            text_meta_written = true;
            Some(bytes)
        }
        ExtensionsMode::Sidecar => {
            let payload = build_text_meta_payload(song);
            let sidecar_path = sidecar_path_for(path);
            save_sidecar(&sidecar_path, &payload)?;
            sidecar_written = Some(sidecar_path);
            None
        }
        ExtensionsMode::None => None,
    };

    let mut tracks: Vec<Vec<TrackEvent<'_>>> = Vec::with_capacity(assignments.len() + 1);
    tracks.push(build_meta_track(
        song,
        text_meta_bytes.as_deref(),
        &mut warnings,
    ));
    for assignment in &assignments {
        tracks.push(build_channel_track(assignment, ppq, &mut warnings)?);
    }

    let header = Header::new(Format::Parallel, Timing::Metrical(u15::new(ppq)));
    let smf = Smf { header, tracks };
    let mut buf: Vec<u8> = Vec::new();
    smf.write(&mut buf)
        .map_err(|e| MidiError::Parse(format!("MIDI serialize failed: {e:?}")))?;
    std::fs::write(path, &buf).map_err(MidiError::Io)?;

    Ok(MidiExportOutcome {
        format: "Type 1",
        ppq,
        track_count: assignments.len(),
        text_meta_written,
        sidecar_written,
        warnings,
    })
}

/// track ごとの channel 割当結果 (= 1 MTrk 1 channel)。
struct ChannelAssignment<'a> {
    track: &'a Track,
    channel_index: u8,
    is_drum: bool,
    sf_preset: u16,
}

/// `Song.tracks` を ADR L60-L77 の規約で channel に割り当てる。
///
/// - drum (= `bank=128`) は ch10 (idx 9) 固定。 複数あれば `MultipleDrumTracksNotSupported`。
/// - melodic は ch1, ch2, ..., ch9, ch11, ..., ch16 の順 (idx 0-8, 10-15 = 計 15 本)。
///   16 を超えれば `TrackLimitExceeded` で超過 track id を返す。
/// - 各 track の `instrument.params` から SF2 preset を取り出す (export では preset/bank が必須)。
fn assign_channels(song: &Song) -> Result<Vec<ChannelAssignment<'_>>, MidiError> {
    let mut melodic: Vec<&Track> = Vec::new();
    let mut drum_tracks: Vec<&Track> = Vec::new();
    for t in &song.tracks {
        if track_is_drum(t) {
            drum_tracks.push(t);
        } else {
            melodic.push(t);
        }
    }

    if drum_tracks.len() > 1 {
        return Err(MidiError::MultipleDrumTracksNotSupported(
            drum_tracks.iter().map(|t| t.id.clone()).collect(),
        ));
    }
    if melodic.len() > MAX_MELODIC_CHANNELS {
        let overflow_ids: Vec<String> = melodic
            .iter()
            .skip(MAX_MELODIC_CHANNELS)
            .map(|t| t.id.clone())
            .collect();
        return Err(MidiError::TrackLimitExceeded(overflow_ids));
    }

    let drum_track = drum_tracks.first().copied();

    let mut assignments: Vec<ChannelAssignment<'_>> = Vec::with_capacity(song.tracks.len());

    let mut melodic_iter = melodic.into_iter();
    for ch_idx in 0u8..16u8 {
        if ch_idx == DRUM_CHANNEL_INDEX {
            if let Some(d) = drum_track {
                let sf = require_soundfont_params(d)?;
                assignments.push(ChannelAssignment {
                    track: d,
                    channel_index: ch_idx,
                    is_drum: true,
                    sf_preset: sf.preset,
                });
            }
            continue;
        }
        // melodic_iter が枯渇しても break しない (= drum 用に ch10 まで進む必要がある)。
        // 上の TrackLimitExceeded check で melodic <= 15 が保証済なので、 ch16 まで回しても
        // assignments は最大 15 melodic + 1 drum で済む。
        if let Some(t) = melodic_iter.next() {
            let sf = require_soundfont_params(t)?;
            assignments.push(ChannelAssignment {
                track: t,
                channel_index: ch_idx,
                is_drum: false,
                sf_preset: sf.preset,
            });
        }
    }

    // ADR L190: tracks[] 並びは channel index 昇順 (drum がその位置に挟まる)
    // tracks_in_channel_order と一致するはずだが、 安全のため明示 sort はしない (= 上の構築順で既に昇順)。
    debug_assert_eq!(
        assignments
            .iter()
            .map(|a| a.channel_index)
            .collect::<Vec<_>>(),
        {
            let mut v: Vec<u8> = assignments.iter().map(|a| a.channel_index).collect();
            v.sort();
            v
        }
    );
    debug_assert_eq!(
        assignments
            .iter()
            .map(|a| a.track.id.as_str())
            .collect::<Vec<_>>(),
        tracks_in_channel_order(song)
            .iter()
            .map(|t| t.id.as_str())
            .collect::<Vec<_>>()
    );

    Ok(assignments)
}

fn require_soundfont_params(track: &Track) -> Result<SoundFontParams, MidiError> {
    if track.instrument.kind != "soundfont" {
        return Err(MidiError::UnsupportedInstrumentType {
            track_id: track.id.clone(),
            kind: track.instrument.kind.clone(),
        });
    }
    SoundFontParams::from_params(&track.instrument.params).map_err(|e| {
        MidiError::InvalidSoundFontParams {
            track_id: track.id.clone(),
            reason: e.to_string(),
        }
    })
}

/// MTrk 0 (= meta track) を構築する。 順序は ADR L141-L142 通り:
/// 1. (text-meta モードなら) Text Meta JSON を delta=0 で
/// 2. Tempo (FF 51)
/// 3. TimeSignature (FF 58)
/// 4. EndOfTrack
///
/// `text_meta_bytes` の寿命は caller (= `export_song`) のスタックに固定する。 leak は避ける。
fn build_meta_track<'a>(
    song: &Song,
    text_meta_bytes: Option<&'a [u8]>,
    warnings: &mut Vec<MidiExportWarning>,
) -> Vec<TrackEvent<'a>> {
    let mut events: Vec<TrackEvent<'a>> = Vec::with_capacity(4);

    if let Some(bytes) = text_meta_bytes {
        events.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::Text(bytes)),
        });
    }

    let uspq = bpm_to_microseconds_per_quarter(song.metadata.bpm);
    events.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(uspq))),
    });

    let (num, denom_pow2) = encode_time_signature(song.metadata.time_signature, warnings);
    events.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(MetaMessage::TimeSignature(num, denom_pow2, 24, 8)),
    });

    events.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
    });

    events
}

fn bpm_to_microseconds_per_quarter(bpm: u32) -> u32 {
    if bpm == 0 {
        return 500_000; // 120 BPM 相当の安全側 fallback。 validate で bpm >= 20 を強制している
    }
    ((60_000_000.0 / bpm as f64).round() as u32).max(1)
}

/// `[N, D]` を midly の (num, denom_pow2) に変換する。 D が 2 のべきでない場合は近似値 + warning。
fn encode_time_signature(ts: [u32; 2], warnings: &mut Vec<MidiExportWarning>) -> (u8, u8) {
    let num = ts[0].clamp(1, 255) as u8;
    let denom = ts[1].max(1);
    if denom.is_power_of_two() {
        let pow2 = denom.trailing_zeros() as u8;
        (num, pow2.min(7))
    } else {
        let approx_pow2 = (denom as f32).log2().round() as u32;
        let approx_denom = 1u32 << approx_pow2;
        warnings.push(MidiExportWarning {
            code: "TIME_SIGNATURE_DENOM_NOT_POWER_OF_TWO",
            track_id: None,
            message: format!(
                "time_signature denominator {denom} is not a power of two; approximated to {approx_denom}"
            ),
        });
        (num, (approx_pow2 as u8).min(7))
    }
}

/// 1 つの melodic / drum track を 1 MTrk として組み立てる。
///
/// 戻り値は MidiMessage / MetaMessage::EndOfTrack のみで `&[u8]` を含まないため、
/// `'a` は caller 任意で OK (meta track 側の Text bytes 寿命に合わせる)。
fn build_channel_track<'a>(
    assignment: &ChannelAssignment<'_>,
    ppq: u16,
    warnings: &mut Vec<MidiExportWarning>,
) -> Result<Vec<TrackEvent<'a>>, MidiError> {
    let track = assignment.track;
    let ch = u4::new(assignment.channel_index);
    let mut events: Vec<TrackEvent<'a>> = Vec::with_capacity(track.notes.len() * 2 + 4);

    // Program Change:
    //   - melodic: 常に書く (= GM Program = preset)
    //   - drum: ch10 は GM の暗黙約束で drum kit、 preset 0 (= Standard Kit) は書かない、
    //           それ以外は非標準 drum kit として書く (ADR L110-L111)
    let program_value = (assignment.sf_preset as u8).min(127);
    let write_program = if assignment.is_drum {
        program_value != 0
    } else {
        true
    };
    if write_program {
        events.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Midi {
                channel: ch,
                message: MidiMessage::ProgramChange {
                    program: u7::new(program_value),
                },
            },
        });
    }

    // CC7 volume (0.0..=1.0 → 0..=127、 中央 default 0.8 → 102)
    let volume_byte = (track.volume.clamp(0.0, 1.0) * 127.0).round() as u8;
    events.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Midi {
            channel: ch,
            message: MidiMessage::Controller {
                controller: u7::new(CC_VOLUME),
                value: u7::new(volume_byte.min(127)),
            },
        },
    });

    // CC10 pan (-1.0..=1.0 → 1..=127、 中央 0.0 → 64)。
    // import 側の (v - 64) / 63 を逆向きに辿って round-trip 一貫性を確保。
    let pan_byte = (track.pan.clamp(-1.0, 1.0) * 63.0 + 64.0).round() as i32;
    let pan_byte = pan_byte.clamp(0, 127) as u8;
    events.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Midi {
            channel: ch,
            message: MidiMessage::Controller {
                controller: u7::new(CC_PAN),
                value: u7::new(pan_byte),
            },
        },
    });

    // notes を絶対 tick 列に展開 (note_on / note_off の event pair)
    enum Ev {
        On { key: u8, vel: u8 },
        Off { key: u8 },
    }
    let mut abs_events: Vec<(u64, u8, Ev)> = Vec::with_capacity(track.notes.len() * 2);

    let mut sorted_notes: Vec<&Note> = track.notes.iter().collect();
    sorted_notes.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));

    for note in &sorted_notes {
        let midi_key = note_to_midi_key(note, assignment.is_drum, &track.id, warnings)?;
        let start_tick = beat_to_tick(note.t.max(0.0), ppq);
        let end_tick = beat_to_tick((note.t + note.dur).max(note.t + 1.0 / f32::from(ppq)), ppq)
            .max(start_tick + 1);
        let vel = note.vel.min(127);
        // sort key 2nd: off (= 0) を先、 on (= 1) を後に並べる → 同 tick で 「直前ノート終了 → 新ノート開始」 が崩れない
        abs_events.push((start_tick, 1, Ev::On { key: midi_key, vel }));
        abs_events.push((end_tick, 0, Ev::Off { key: midi_key }));
    }
    abs_events.sort_by_key(|(t, ord, _)| (*t, *ord));

    let mut prev_abs: u64 = 0;
    for (abs_tick, _, ev) in abs_events {
        let delta = abs_tick.saturating_sub(prev_abs);
        prev_abs = abs_tick;
        let delta_u28 = u28::new(delta.min(u28::max_value().as_int() as u64) as u32);
        let message = match ev {
            Ev::On { key, vel } => MidiMessage::NoteOn {
                key: u7::new(key.min(127)),
                vel: u7::new(vel),
            },
            Ev::Off { key } => MidiMessage::NoteOff {
                key: u7::new(key.min(127)),
                vel: u7::new(NOTE_OFF_VELOCITY),
            },
        };
        events.push(TrackEvent {
            delta: delta_u28,
            kind: TrackEventKind::Midi {
                channel: ch,
                message,
            },
        });
    }

    events.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
    });

    Ok(events)
}

fn beat_to_tick(beat: f32, ppq: u16) -> u64 {
    let t = (beat as f64) * f64::from(ppq);
    t.max(0.0).round() as u64
}

/// `Note` の `Pitch` を MIDI 番号に変換する。
///
/// - drum track (= ch10): 要素名キー (DRUM_KEY_MIDI_MAP) を最優先、 該当なければ note 名 (= "C4" 等) として解釈
/// - melodic track: 要素名キーは error (= drum 専用と区別する)
fn note_to_midi_key(
    note: &Note,
    is_drum: bool,
    track_id: &str,
    _warnings: &mut Vec<MidiExportWarning>,
) -> Result<u8, MidiError> {
    match &note.pitch {
        Pitch::Midi(n) => Ok(*n),
        Pitch::Name(s) => {
            if is_drum {
                if let Some(n) = drum_key_to_midi(s) {
                    return Ok(n);
                }
            }
            parse_note_name(s).map_err(|e| MidiError::InvalidNotePitch {
                track_id: track_id.to_string(),
                reason: if is_drum {
                    format!(
                        "drum pitch {s:?} is neither a known drum key nor a parseable note name: {e}"
                    )
                } else {
                    format!("note pitch {s:?} could not be parsed: {e}")
                },
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Effect, Instrument, Metadata, Note, Pitch, Track};
    use crate::{import_midi, ExtensionsRecovered, MidiImportOptions};
    use serde_json::{json, Map};

    fn melodic_track(id: &str, preset: u16, notes: Vec<Note>) -> Track {
        let mut params = Map::new();
        params.insert("file".into(), json!("test.sf2"));
        params.insert("preset".into(), json!(preset));
        params.insert("bank".into(), json!(0));
        Track {
            id: id.into(),
            name: id.into(),
            instrument: Instrument {
                kind: "soundfont".into(),
                params,
            },
            volume: 0.8,
            pan: 0.0,
            mute: false,
            solo: false,
            fx: vec![],
            notes,
        }
    }

    fn drum_track(id: &str, notes: Vec<Note>) -> Track {
        let mut params = Map::new();
        params.insert("file".into(), json!("test.sf2"));
        params.insert("preset".into(), json!(0));
        params.insert("bank".into(), json!(128));
        Track {
            id: id.into(),
            name: id.into(),
            instrument: Instrument {
                kind: "soundfont".into(),
                params,
            },
            volume: 0.9,
            pan: 0.0,
            mute: false,
            solo: false,
            fx: vec![],
            notes,
        }
    }

    fn empty_song() -> Song {
        Song {
            version: SCHEMA_VERSION.to_string(),
            metadata: Metadata {
                name: "t".into(),
                bpm: 120,
                key: None,
                time_signature: [4, 4],
                master_gain: 1.0,
                created_at: None,
                tags: Vec::new(),
            },
            tracks: vec![],
        }
    }

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!(
            "codetta-midi-export-{nanos}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn export_with_no_tracks_writes_only_meta_track() {
        let song = empty_song();
        let dir = tempdir();
        let path = dir.join("empty.mid");

        let outcome = export_song(&song, &path, &MidiExportOptions::default()).expect("export");
        assert_eq!(outcome.format, "Type 1");
        assert_eq!(outcome.ppq, DEFAULT_PPQ);
        assert_eq!(outcome.track_count, 0);
        assert!(outcome.text_meta_written);

        let bytes = std::fs::read(&path).expect("file written");
        let smf = midly::Smf::parse(&bytes).expect("parse");
        // MTrk 0 (= meta) のみ
        assert_eq!(smf.tracks.len(), 1);
    }

    #[test]
    fn export_assigns_drum_to_channel_10() {
        let mut song = empty_song();
        song.tracks.push(melodic_track(
            "lead",
            81,
            vec![Note {
                t: 0.0,
                pitch: Pitch::Midi(60),
                dur: 1.0,
                vel: 100,
            }],
        ));
        song.tracks.push(drum_track(
            "drums",
            vec![Note {
                t: 0.0,
                pitch: Pitch::Name("kick".into()),
                dur: 0.25,
                vel: 110,
            }],
        ));

        let dir = tempdir();
        let path = dir.join("two-tracks.mid");
        let outcome = export_song(&song, &path, &MidiExportOptions::default()).expect("export");
        assert_eq!(outcome.track_count, 2);

        let bytes = std::fs::read(&path).unwrap();
        let smf = midly::Smf::parse(&bytes).unwrap();
        assert_eq!(smf.tracks.len(), 3); // meta + 2 channel tracks

        // ch1 melodic (= idx 0) を含む MTrk 1
        let mel_ch_idx = first_channel_index(&smf.tracks[1]).expect("melodic channel");
        assert_eq!(mel_ch_idx, 0);
        // drum (= idx 9) を含む MTrk 2
        let drum_ch_idx = first_channel_index(&smf.tracks[2]).expect("drum channel");
        assert_eq!(drum_ch_idx, 9);
    }

    fn first_channel_index(track: &[midly::TrackEvent<'_>]) -> Option<u8> {
        track.iter().find_map(|ev| match ev.kind {
            TrackEventKind::Midi { channel, .. } => Some(channel.as_int()),
            _ => None,
        })
    }

    #[test]
    fn export_rejects_multiple_drum_tracks() {
        let mut song = empty_song();
        song.tracks.push(drum_track(
            "drums-1",
            vec![Note {
                t: 0.0,
                pitch: Pitch::Midi(36),
                dur: 0.25,
                vel: 100,
            }],
        ));
        song.tracks.push(drum_track("drums-2", vec![]));

        let dir = tempdir();
        let path = dir.join("multi-drum.mid");
        let err = export_song(&song, &path, &MidiExportOptions::default()).unwrap_err();
        match err {
            MidiError::MultipleDrumTracksNotSupported(ids) => {
                assert_eq!(ids, vec!["drums-1".to_string(), "drums-2".to_string()]);
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(!path.exists(), "no file should be written on error");
    }

    #[test]
    fn export_rejects_more_than_15_melodic_tracks() {
        let mut song = empty_song();
        for i in 0..16 {
            song.tracks
                .push(melodic_track(&format!("mel-{i}"), 0, vec![]));
        }
        let dir = tempdir();
        let path = dir.join("limit.mid");
        let err = export_song(&song, &path, &MidiExportOptions::default()).unwrap_err();
        match err {
            MidiError::TrackLimitExceeded(ids) => {
                // 16 番目 (index 15) が overflow
                assert_eq!(ids, vec!["mel-15".to_string()]);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn export_rejects_non_soundfont_instrument() {
        let mut song = empty_song();
        let mut bad = melodic_track("legacy", 0, vec![]);
        bad.instrument.kind = "sin".into();
        bad.instrument.params.clear();
        song.tracks.push(bad);

        let dir = tempdir();
        let path = dir.join("legacy.mid");
        let err = export_song(&song, &path, &MidiExportOptions::default()).unwrap_err();
        assert!(matches!(
            err,
            MidiError::UnsupportedInstrumentType { ref track_id, .. } if track_id == "legacy"
        ));
    }

    #[test]
    fn export_drum_key_resolves_to_gm_drum_midi_number() {
        let mut song = empty_song();
        song.tracks.push(drum_track(
            "d",
            vec![
                Note {
                    t: 0.0,
                    pitch: Pitch::Name("kick".into()),
                    dur: 0.25,
                    vel: 110,
                },
                Note {
                    t: 1.0,
                    pitch: Pitch::Name("snare".into()),
                    dur: 0.25,
                    vel: 115,
                },
            ],
        ));

        let dir = tempdir();
        let path = dir.join("drum-keys.mid");
        export_song(&song, &path, &MidiExportOptions::default()).expect("export");

        let bytes = std::fs::read(&path).unwrap();
        let smf = midly::Smf::parse(&bytes).unwrap();
        // MTrk 1 = drum (1 番目の channel track)
        let drum_track = &smf.tracks[1];
        let keys: Vec<u8> = drum_track
            .iter()
            .filter_map(|ev| match ev.kind {
                TrackEventKind::Midi {
                    message: MidiMessage::NoteOn { key, .. },
                    ..
                } => Some(key.as_int()),
                _ => None,
            })
            .collect();
        assert_eq!(keys, vec![36, 38]); // kick=36, snare=38
    }

    #[test]
    fn export_then_import_roundtrips_basic_song() {
        let mut song = empty_song();
        song.metadata.master_gain = 2.0;
        song.metadata.key = Some("Am".into());
        song.metadata.tags = vec!["test".into()];
        song.tracks.push(Track {
            id: "lead".into(),
            name: "Saw Lead".into(),
            instrument: Instrument {
                kind: "soundfont".into(),
                params: {
                    let mut m = Map::new();
                    m.insert("file".into(), json!("test.sf2"));
                    m.insert("preset".into(), json!(81));
                    m.insert("bank".into(), json!(0));
                    m
                },
            },
            volume: 0.8,
            pan: 0.0,
            mute: false,
            solo: false,
            fx: vec![Effect {
                kind: "reverb".into(),
                params: {
                    let mut m = Map::new();
                    m.insert("mix".into(), json!(0.2));
                    m
                },
            }],
            notes: vec![
                Note {
                    t: 0.0,
                    pitch: Pitch::Midi(60),
                    dur: 1.0,
                    vel: 100,
                },
                Note {
                    t: 1.0,
                    pitch: Pitch::Midi(64),
                    dur: 1.0,
                    vel: 100,
                },
            ],
        });
        song.tracks.push(drum_track(
            "drums",
            vec![Note {
                t: 0.0,
                pitch: Pitch::Name("kick".into()),
                dur: 0.25,
                vel: 110,
            }],
        ));

        let dir = tempdir();
        let mid = dir.join("rt.mid");
        export_song(&song, &mid, &MidiExportOptions::default()).expect("export");

        let outcome =
            import_midi(&mid, &MidiImportOptions::default()).expect("import after export");
        let ExtensionsRecovered {
            master_gain,
            fx,
            soundfont_params,
            source,
        } = outcome.extensions_recovered;
        assert_eq!(source, "text-meta", "text-meta should round-trip");
        assert!(master_gain);
        assert!(fx);
        assert!(soundfont_params);

        let rt = &outcome.song;
        assert_eq!(rt.version, "0.2");
        assert_eq!(rt.metadata.bpm, song.metadata.bpm);
        assert!((rt.metadata.master_gain - 2.0).abs() < 1e-3);
        assert_eq!(rt.metadata.key.as_deref(), Some("Am"));
        assert_eq!(rt.metadata.tags, vec!["test".to_string()]);

        assert_eq!(rt.tracks.len(), 2);
        // lead
        assert_eq!(rt.tracks[0].id, "lead");
        assert_eq!(rt.tracks[0].instrument.kind, "soundfont");
        assert_eq!(rt.tracks[0].instrument.params["preset"], 81);
        assert_eq!(rt.tracks[0].instrument.params["bank"], 0);
        assert_eq!(rt.tracks[0].fx.len(), 1);
        assert_eq!(rt.tracks[0].fx[0].kind, "reverb");
        assert_eq!(rt.tracks[0].notes.len(), 2);
        assert_eq!(rt.tracks[0].notes[0].pitch, Pitch::Midi(60));
        // drum (要素名キーは round-trip で数値固定: ADR L120)
        assert_eq!(rt.tracks[1].id, "drums");
        assert_eq!(rt.tracks[1].instrument.params["bank"], 128);
        assert_eq!(rt.tracks[1].notes[0].pitch, Pitch::Midi(36));
    }

    #[test]
    fn export_with_extensions_none_omits_text_meta() {
        let mut song = empty_song();
        song.tracks.push(melodic_track(
            "lead",
            0,
            vec![Note {
                t: 0.0,
                pitch: Pitch::Midi(60),
                dur: 1.0,
                vel: 100,
            }],
        ));
        let dir = tempdir();
        let mid = dir.join("none.mid");
        let outcome = export_song(
            &song,
            &mid,
            &MidiExportOptions {
                extensions: ExtensionsMode::None,
                ppq: DEFAULT_PPQ,
            },
        )
        .expect("export");
        assert!(!outcome.text_meta_written);
        assert!(outcome.sidecar_written.is_none());

        // import 側でも text-meta source は "none" になり、 master_gain 等は default 復元
        let outcome =
            import_midi(&mid, &MidiImportOptions::default()).expect("import after none export");
        assert_eq!(outcome.extensions_recovered.source, "none");
        assert!(!outcome.extensions_recovered.master_gain);
    }

    #[test]
    fn export_with_extensions_sidecar_writes_separate_file() {
        let mut song = empty_song();
        song.metadata.master_gain = 1.5;
        song.tracks.push(melodic_track(
            "lead",
            81,
            vec![Note {
                t: 0.0,
                pitch: Pitch::Midi(60),
                dur: 1.0,
                vel: 100,
            }],
        ));
        let dir = tempdir();
        let mid = dir.join("song.mid");
        let outcome = export_song(
            &song,
            &mid,
            &MidiExportOptions {
                extensions: ExtensionsMode::Sidecar,
                ppq: DEFAULT_PPQ,
            },
        )
        .expect("export");
        assert!(!outcome.text_meta_written);
        let sidecar = outcome.sidecar_written.expect("sidecar path");
        assert_eq!(sidecar, dir.join("song.codetta.meta.json"));
        assert!(sidecar.exists());

        // import 側でも sidecar から master_gain を復元できる
        let outcome = import_midi(
            &mid,
            &MidiImportOptions {
                extensions: ExtensionsMode::TextMeta, // fallback で sidecar を読む
                sf2_file: None,
                song_name: None,
            },
        )
        .expect("import after sidecar export");
        assert_eq!(outcome.extensions_recovered.source, "sidecar");
        assert!((outcome.song.metadata.master_gain - 1.5).abs() < 1e-3);
    }

    #[test]
    fn export_round_trip_is_fixed_point_after_three_passes() {
        // ADR L372: 「三度回し」 = 1 度 import → export → import で得た song を再 export → 再 import しても
        // 同じ song になる (= 関数の固定点)。
        let mut song = empty_song();
        song.tracks.push(melodic_track(
            "lead",
            81,
            vec![
                Note {
                    t: 0.0,
                    pitch: Pitch::Midi(60),
                    dur: 1.0,
                    vel: 100,
                },
                Note {
                    t: 1.0,
                    pitch: Pitch::Midi(64),
                    dur: 0.5,
                    vel: 90,
                },
            ],
        ));
        let dir = tempdir();
        let mid_1 = dir.join("p1.mid");
        export_song(&song, &mid_1, &MidiExportOptions::default()).expect("export 1");
        let song_2 = import_midi(&mid_1, &MidiImportOptions::default())
            .expect("import 1")
            .song;

        let mid_2 = dir.join("p2.mid");
        export_song(&song_2, &mid_2, &MidiExportOptions::default()).expect("export 2");
        let song_3 = import_midi(&mid_2, &MidiImportOptions::default())
            .expect("import 2")
            .song;

        assert_eq!(song_2.version, song_3.version);
        assert_eq!(song_2.tracks.len(), song_3.tracks.len());
        for (a, b) in song_2.tracks.iter().zip(song_3.tracks.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.notes.len(), b.notes.len());
            for (na, nb) in a.notes.iter().zip(b.notes.iter()) {
                assert_eq!(na.pitch, nb.pitch);
                assert!((na.t - nb.t).abs() < 1e-3);
                assert!((na.dur - nb.dur).abs() < 1e-3);
                assert_eq!(na.vel, nb.vel);
            }
        }
    }
}
