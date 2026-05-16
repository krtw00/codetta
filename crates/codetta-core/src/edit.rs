//! Song に対する純粋な編集操作。
//!
//! CLI / MCP server から呼ぶ「load → 編集 → validate → save」テンプレの中央。
//! 副作用なし: Song を直接 mutate するか、 必要なら新しい Note 列を返す。
//!
//! ここでは「型 / ID 整合性」 のみチェックする (例: 重複 id, 存在しない track id)。
//! 値域や schema 違反は [`crate::validate`] が担当する。 呼び出し側は edit が成功した
//! 後に validate を回す。

use serde::Deserialize;

use crate::error::CodettaError;
use crate::model::{Effect, Instrument, Note, Pitch, Song, Track};

/// 新規トラックを追加する。
///
/// `id` が既存トラックと重複すれば [`CodettaError::TrackIdDuplicate`]。
pub fn add_track(song: &mut Song, track: Track) -> Result<(), CodettaError> {
    if song.tracks.iter().any(|t| t.id == track.id) {
        return Err(CodettaError::TrackIdDuplicate(track.id));
    }
    song.tracks.push(track);
    Ok(())
}

/// 指定 ID のトラックを削除する。
///
/// 存在しなければ [`CodettaError::TrackNotFound`]。
pub fn remove_track(song: &mut Song, track_id: &str) -> Result<(), CodettaError> {
    let pos = song
        .tracks
        .iter()
        .position(|t| t.id == track_id)
        .ok_or_else(|| CodettaError::TrackNotFound(track_id.to_string()))?;
    song.tracks.remove(pos);
    Ok(())
}

/// `track_id` を可変参照で取得する小ヘルパー。
pub fn track_mut<'a>(song: &'a mut Song, track_id: &str) -> Result<&'a mut Track, CodettaError> {
    song.tracks
        .iter_mut()
        .find(|t| t.id == track_id)
        .ok_or_else(|| CodettaError::TrackNotFound(track_id.to_string()))
}

/// トラックのノート列を全置換する。 戻り値は新しい note_count。
pub fn set_notes(song: &mut Song, track_id: &str, notes: Vec<Note>) -> Result<usize, CodettaError> {
    let track = track_mut(song, track_id)?;
    track.notes = notes;
    sort_notes(&mut track.notes);
    Ok(track.notes.len())
}

/// ノートを追加する。 既存と完全一致する (`t`, `pitch`, `dur`) ノートはスキップ。
/// 戻り値: `(added, skipped_duplicates, total)`。
pub fn add_notes(
    song: &mut Song,
    track_id: &str,
    notes: Vec<Note>,
) -> Result<(usize, usize, usize), CodettaError> {
    let track = track_mut(song, track_id)?;
    let mut added = 0;
    let mut skipped = 0;
    for n in notes {
        if track
            .notes
            .iter()
            .any(|m| m.t == n.t && m.pitch == n.pitch && m.dur == n.dur)
        {
            skipped += 1;
            continue;
        }
        track.notes.push(n);
        added += 1;
    }
    sort_notes(&mut track.notes);
    Ok((added, skipped, track.notes.len()))
}

/// トラックのノートを全削除。 戻り値: 削除した件数。
pub fn clear_notes(song: &mut Song, track_id: &str) -> Result<usize, CodettaError> {
    let track = track_mut(song, track_id)?;
    let removed = track.notes.len();
    track.notes.clear();
    Ok(removed)
}

/// トラックの楽器を差し替える。 戻り値: 旧 instrument の type。
pub fn set_instrument(
    song: &mut Song,
    track_id: &str,
    instrument: Instrument,
) -> Result<String, CodettaError> {
    let track = track_mut(song, track_id)?;
    let prev = std::mem::replace(&mut track.instrument, instrument);
    Ok(prev.kind)
}

/// トラックのエフェクトチェーンを全置換。 戻り値: 新しい fx 数。
pub fn set_fx(
    song: &mut Song,
    track_id: &str,
    fx: Vec<Effect>,
) -> Result<usize, CodettaError> {
    let track = track_mut(song, track_id)?;
    track.fx = fx;
    Ok(track.fx.len())
}

/// `edit-notes` の 1 操作。 03-cli.md と対応。
///
/// `range` は `[t_start, t_end)` (半開区間)。 ノートの `t` がこの範囲に
/// 入っているかで適用判定する。 None は「全ノート対象」。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum NoteOp {
    /// 半音単位で移調 (drum_kit トラックは適用しても意味がないので skip)。
    Transpose {
        semitones: i32,
        #[serde(default)]
        range: Option<[f32; 2]>,
    },
    /// 時間方向にビート単位でシフト。 結果が負になるノートはエラー。
    ShiftTime {
        beats: f32,
        #[serde(default)]
        range: Option<[f32; 2]>,
    },
    /// 時間軸を引き伸ばし / 縮め (t と dur の両方に factor を掛ける)。
    /// range は持たない — 全体に作用する。
    ScaleTime { factor: f32 },
    /// ベロシティ一括変更 (0..=127 にクランプ)。
    SetVelocity {
        vel: u8,
        #[serde(default)]
        range: Option<[f32; 2]>,
    },
    /// `t` をグリッドに量子化 (例: grid=0.25 → 1/16 拍)。 dur は変えない。
    Quantize { grid: f32 },
    /// 範囲内のノートを削除。
    DeleteRange { range: [f32; 2] },
}

/// `edit-notes` の戻り値統計。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EditNotesStats {
    pub ops_applied: usize,
    pub notes_affected: usize,
}

/// 複数の op を順に適用する。 一つでも実行不能 (例: shift で t<0) ならエラー。
pub fn edit_notes(
    song: &mut Song,
    track_id: &str,
    ops: &[NoteOp],
) -> Result<EditNotesStats, CodettaError> {
    let track = track_mut(song, track_id)?;
    let mut stats = EditNotesStats::default();
    for op in ops {
        let n = apply_note_op(&mut track.notes, op)?;
        stats.ops_applied += 1;
        stats.notes_affected += n;
    }
    sort_notes(&mut track.notes);
    Ok(stats)
}

fn apply_note_op(notes: &mut Vec<Note>, op: &NoteOp) -> Result<usize, CodettaError> {
    match op {
        NoteOp::Transpose { semitones, range } => {
            let mut affected = 0;
            for n in notes.iter_mut() {
                if !in_range(n.t, range) {
                    continue;
                }
                match n.pitch.as_midi() {
                    Ok(m) => {
                        let new_m = m as i32 + *semitones;
                        if !(0..=127).contains(&new_m) {
                            return Err(CodettaError::Render(format!(
                                "transpose: MIDI {m} + {semitones} = {new_m} is out of 0..=127"
                            )));
                        }
                        n.pitch = Pitch::Midi(new_m as u8);
                        affected += 1;
                    }
                    Err(_) => {
                        // drum key 等は skip
                    }
                }
            }
            Ok(affected)
        }
        NoteOp::ShiftTime { beats, range } => {
            let mut affected = 0;
            for n in notes.iter_mut() {
                if !in_range(n.t, range) {
                    continue;
                }
                let new_t = n.t + *beats;
                if new_t < 0.0 {
                    return Err(CodettaError::Render(format!(
                        "shift_time: note at t={} would become negative ({new_t})",
                        n.t
                    )));
                }
                n.t = new_t;
                affected += 1;
            }
            Ok(affected)
        }
        NoteOp::ScaleTime { factor } => {
            if !factor.is_finite() || *factor <= 0.0 {
                return Err(CodettaError::Render(format!(
                    "scale_time: factor must be positive finite, got {factor}"
                )));
            }
            for n in notes.iter_mut() {
                n.t *= *factor;
                n.dur *= *factor;
            }
            Ok(notes.len())
        }
        NoteOp::SetVelocity { vel, range } => {
            let clamped = (*vel).min(127);
            let mut affected = 0;
            for n in notes.iter_mut() {
                if !in_range(n.t, range) {
                    continue;
                }
                n.vel = clamped;
                affected += 1;
            }
            Ok(affected)
        }
        NoteOp::Quantize { grid } => {
            if !grid.is_finite() || *grid <= 0.0 {
                return Err(CodettaError::Render(format!(
                    "quantize: grid must be positive finite, got {grid}"
                )));
            }
            for n in notes.iter_mut() {
                let g = *grid;
                n.t = (n.t / g).round() * g;
            }
            Ok(notes.len())
        }
        NoteOp::DeleteRange { range } => {
            let before = notes.len();
            notes.retain(|n| !(n.t >= range[0] && n.t < range[1]));
            Ok(before - notes.len())
        }
    }
}

fn in_range(t: f32, range: &Option<[f32; 2]>) -> bool {
    match range {
        None => true,
        Some([a, b]) => t >= *a && t < *b,
    }
}

/// ノートを `t` 昇順、 同 `t` ならピッチで安定ソート。
fn sort_notes(notes: &mut [Note]) {
    notes.sort_by(|a, b| {
        a.t.partial_cmp(&b.t)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let ma = a.pitch.as_midi().ok();
                let mb = b.pitch.as_midi().ok();
                ma.cmp(&mb)
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Instrument, Pitch};

    fn new_track(id: &str) -> Track {
        Track {
            id: id.into(),
            name: id.into(),
            instrument: Instrument::new("sin"),
            volume: 0.8,
            pan: 0.0,
            mute: false,
            solo: false,
            fx: vec![],
            notes: vec![],
        }
    }

    fn note(t: f32, pitch: &str, dur: f32, vel: u8) -> Note {
        Note {
            t,
            pitch: Pitch::Name(pitch.into()),
            dur,
            vel,
        }
    }

    #[test]
    fn add_track_appends() {
        let mut s = Song::new("t", 120, None);
        add_track(&mut s, new_track("lead")).unwrap();
        assert_eq!(s.tracks.len(), 1);
        assert_eq!(s.tracks[0].id, "lead");
    }

    #[test]
    fn add_track_rejects_duplicate_id() {
        let mut s = Song::new("t", 120, None);
        add_track(&mut s, new_track("lead")).unwrap();
        let err = add_track(&mut s, new_track("lead")).unwrap_err();
        assert!(matches!(err, CodettaError::TrackIdDuplicate(id) if id == "lead"));
    }

    #[test]
    fn remove_track_works() {
        let mut s = Song::new("t", 120, None);
        add_track(&mut s, new_track("lead")).unwrap();
        add_track(&mut s, new_track("bass")).unwrap();
        remove_track(&mut s, "lead").unwrap();
        assert_eq!(s.tracks.len(), 1);
        assert_eq!(s.tracks[0].id, "bass");
    }

    #[test]
    fn remove_track_not_found() {
        let mut s = Song::new("t", 120, None);
        let err = remove_track(&mut s, "ghost").unwrap_err();
        assert!(matches!(err, CodettaError::TrackNotFound(id) if id == "ghost"));
    }

    #[test]
    fn set_notes_replaces_and_sorts() {
        let mut s = Song::new("t", 120, None);
        add_track(&mut s, new_track("lead")).unwrap();
        s.tracks[0].notes.push(note(0.0, "C4", 0.5, 100));
        let n = set_notes(
            &mut s,
            "lead",
            vec![note(1.0, "E4", 0.5, 100), note(0.5, "D4", 0.5, 100)],
        )
        .unwrap();
        assert_eq!(n, 2);
        assert_eq!(s.tracks[0].notes[0].t, 0.5);
        assert_eq!(s.tracks[0].notes[1].t, 1.0);
    }

    #[test]
    fn add_notes_dedups() {
        let mut s = Song::new("t", 120, None);
        add_track(&mut s, new_track("lead")).unwrap();
        set_notes(&mut s, "lead", vec![note(0.0, "C4", 0.5, 100)]).unwrap();
        let (added, skipped, total) = add_notes(
            &mut s,
            "lead",
            vec![
                note(0.0, "C4", 0.5, 100), // dup (vel違いでも skip — t/pitch/dur で同一)
                note(0.5, "D4", 0.5, 100),
                note(1.0, "E4", 0.5, 100),
            ],
        )
        .unwrap();
        assert_eq!(added, 2);
        assert_eq!(skipped, 1);
        assert_eq!(total, 3);
    }

    #[test]
    fn clear_notes_returns_count() {
        let mut s = Song::new("t", 120, None);
        add_track(&mut s, new_track("lead")).unwrap();
        set_notes(
            &mut s,
            "lead",
            vec![note(0.0, "C4", 0.5, 100), note(0.5, "D4", 0.5, 100)],
        )
        .unwrap();
        let removed = clear_notes(&mut s, "lead").unwrap();
        assert_eq!(removed, 2);
        assert!(s.tracks[0].notes.is_empty());
    }

    #[test]
    fn set_instrument_returns_prev() {
        let mut s = Song::new("t", 120, None);
        add_track(&mut s, new_track("lead")).unwrap();
        let prev = set_instrument(&mut s, "lead", Instrument::new("saw_lead")).unwrap();
        assert_eq!(prev, "sin");
        assert_eq!(s.tracks[0].instrument.kind, "saw_lead");
    }

    #[test]
    fn set_fx_replaces_chain() {
        let mut s = Song::new("t", 120, None);
        add_track(&mut s, new_track("lead")).unwrap();
        let count = set_fx(
            &mut s,
            "lead",
            vec![
                Effect {
                    kind: "lowpass".into(),
                    params: Default::default(),
                },
                Effect {
                    kind: "reverb".into(),
                    params: Default::default(),
                },
            ],
        )
        .unwrap();
        assert_eq!(count, 2);
        assert_eq!(s.tracks[0].fx[0].kind, "lowpass");
    }

    fn make_song_with_notes() -> Song {
        let mut s = Song::new("t", 120, None);
        add_track(&mut s, new_track("lead")).unwrap();
        set_notes(
            &mut s,
            "lead",
            vec![
                note(0.0, "C4", 0.5, 100),
                note(0.5, "D4", 0.5, 100),
                note(1.0, "E4", 0.5, 100),
                note(2.0, "G4", 0.5, 80),
            ],
        )
        .unwrap();
        s
    }

    #[test]
    fn edit_notes_transpose_full() {
        let mut s = make_song_with_notes();
        let stats = edit_notes(
            &mut s,
            "lead",
            &[NoteOp::Transpose {
                semitones: -12,
                range: None,
            }],
        )
        .unwrap();
        assert_eq!(stats.ops_applied, 1);
        assert_eq!(stats.notes_affected, 4);
        assert_eq!(s.tracks[0].notes[0].pitch.as_midi().unwrap(), 60 - 12);
        assert_eq!(s.tracks[0].notes[3].pitch.as_midi().unwrap(), 67 - 12);
    }

    #[test]
    fn edit_notes_transpose_range_partial() {
        let mut s = make_song_with_notes();
        // 0.5..1.5 → ノート 2 つ (t=0.5, 1.0)
        let stats = edit_notes(
            &mut s,
            "lead",
            &[NoteOp::Transpose {
                semitones: 12,
                range: Some([0.5, 1.5]),
            }],
        )
        .unwrap();
        assert_eq!(stats.notes_affected, 2);
        assert_eq!(s.tracks[0].notes[0].pitch.as_midi().unwrap(), 60); // unchanged
        assert_eq!(s.tracks[0].notes[1].pitch.as_midi().unwrap(), 62 + 12);
        assert_eq!(s.tracks[0].notes[2].pitch.as_midi().unwrap(), 64 + 12);
        assert_eq!(s.tracks[0].notes[3].pitch.as_midi().unwrap(), 67); // unchanged
    }

    #[test]
    fn edit_notes_transpose_out_of_range_fails() {
        let mut s = make_song_with_notes();
        let err = edit_notes(
            &mut s,
            "lead",
            &[NoteOp::Transpose {
                semitones: 200,
                range: None,
            }],
        )
        .unwrap_err();
        assert!(matches!(err, CodettaError::Render(_)));
    }

    #[test]
    fn edit_notes_shift_time() {
        let mut s = make_song_with_notes();
        edit_notes(
            &mut s,
            "lead",
            &[NoteOp::ShiftTime {
                beats: 1.0,
                range: None,
            }],
        )
        .unwrap();
        assert_eq!(s.tracks[0].notes[0].t, 1.0);
        assert_eq!(s.tracks[0].notes[3].t, 3.0);
    }

    #[test]
    fn edit_notes_shift_time_negative_fails() {
        let mut s = make_song_with_notes();
        let err = edit_notes(
            &mut s,
            "lead",
            &[NoteOp::ShiftTime {
                beats: -10.0,
                range: None,
            }],
        )
        .unwrap_err();
        assert!(matches!(err, CodettaError::Render(_)));
    }

    #[test]
    fn edit_notes_scale_time() {
        let mut s = make_song_with_notes();
        edit_notes(&mut s, "lead", &[NoteOp::ScaleTime { factor: 2.0 }]).unwrap();
        assert_eq!(s.tracks[0].notes[1].t, 1.0);
        assert_eq!(s.tracks[0].notes[1].dur, 1.0);
        assert_eq!(s.tracks[0].notes[3].t, 4.0);
    }

    #[test]
    fn edit_notes_scale_time_bad_factor() {
        let mut s = make_song_with_notes();
        let err = edit_notes(&mut s, "lead", &[NoteOp::ScaleTime { factor: -1.0 }]).unwrap_err();
        assert!(matches!(err, CodettaError::Render(_)));
    }

    #[test]
    fn edit_notes_set_velocity() {
        let mut s = make_song_with_notes();
        edit_notes(
            &mut s,
            "lead",
            &[NoteOp::SetVelocity {
                vel: 90,
                range: None,
            }],
        )
        .unwrap();
        for n in &s.tracks[0].notes {
            assert_eq!(n.vel, 90);
        }
    }

    #[test]
    fn edit_notes_quantize() {
        let mut s = Song::new("t", 120, None);
        add_track(&mut s, new_track("lead")).unwrap();
        set_notes(
            &mut s,
            "lead",
            vec![
                note(0.03, "C4", 0.5, 100),
                note(0.51, "D4", 0.5, 100),
                note(1.13, "E4", 0.5, 100),
            ],
        )
        .unwrap();
        edit_notes(&mut s, "lead", &[NoteOp::Quantize { grid: 0.25 }]).unwrap();
        // 0.03 → 0.0, 0.51 → 0.5, 1.13 → 1.25
        assert!((s.tracks[0].notes[0].t - 0.0).abs() < 1e-5);
        assert!((s.tracks[0].notes[1].t - 0.5).abs() < 1e-5);
        assert!((s.tracks[0].notes[2].t - 1.25).abs() < 1e-5);
    }

    #[test]
    fn edit_notes_delete_range() {
        let mut s = make_song_with_notes();
        let stats = edit_notes(
            &mut s,
            "lead",
            &[NoteOp::DeleteRange { range: [0.5, 1.5] }],
        )
        .unwrap();
        // t=0.5 と t=1.0 が消える
        assert_eq!(stats.notes_affected, 2);
        assert_eq!(s.tracks[0].notes.len(), 2);
        assert_eq!(s.tracks[0].notes[0].t, 0.0);
        assert_eq!(s.tracks[0].notes[1].t, 2.0);
    }

    #[test]
    fn edit_notes_chains_ops() {
        let mut s = make_song_with_notes();
        let stats = edit_notes(
            &mut s,
            "lead",
            &[
                NoteOp::Transpose {
                    semitones: -12,
                    range: None,
                },
                NoteOp::SetVelocity {
                    vel: 100,
                    range: None,
                },
            ],
        )
        .unwrap();
        assert_eq!(stats.ops_applied, 2);
        // 1 op 目で 4 + 2 op 目で 4 = 8 (notes_affected はカウントの合計)
        assert_eq!(stats.notes_affected, 8);
    }

    #[test]
    fn note_op_deserializes_from_json() {
        let ops: Vec<NoteOp> = serde_json::from_str(
            r#"[
                {"op":"transpose","semitones":-12},
                {"op":"shift_time","beats":1.0,"range":[0,2]},
                {"op":"scale_time","factor":0.5},
                {"op":"set_velocity","vel":90},
                {"op":"quantize","grid":0.25},
                {"op":"delete_range","range":[2,4]}
            ]"#,
        )
        .unwrap();
        assert_eq!(ops.len(), 6);
    }

    #[test]
    fn notes_ops_on_missing_track_return_err() {
        let mut s = Song::new("t", 120, None);
        assert!(matches!(
            set_notes(&mut s, "ghost", vec![]).unwrap_err(),
            CodettaError::TrackNotFound(_)
        ));
        assert!(matches!(
            add_notes(&mut s, "ghost", vec![]).unwrap_err(),
            CodettaError::TrackNotFound(_)
        ));
        assert!(matches!(
            clear_notes(&mut s, "ghost").unwrap_err(),
            CodettaError::TrackNotFound(_)
        ));
    }
}
