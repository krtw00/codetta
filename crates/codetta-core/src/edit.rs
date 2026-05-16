//! Song に対する純粋な編集操作。
//!
//! CLI / MCP server から呼ぶ「load → 編集 → validate → save」テンプレの中央。
//! 副作用なし: Song を直接 mutate するか、 必要なら新しい Note 列を返す。
//!
//! ここでは「型 / ID 整合性」 のみチェックする (例: 重複 id, 存在しない track id)。
//! 値域や schema 違反は [`crate::validate`] が担当する。 呼び出し側は edit が成功した
//! 後に validate を回す。

use crate::error::CodettaError;
use crate::model::{Effect, Instrument, Note, Song, Track};

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
