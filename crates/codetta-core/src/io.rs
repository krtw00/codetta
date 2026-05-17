use std::path::Path;

use crate::error::CodettaError;
use crate::model::Song;

/// `.codetta` ファイルを読み込み、 サポートバージョンを検証する。
///
/// validate() は呼ばない (CLI 側で必要に応じて呼ぶ)。
pub fn load(path: impl AsRef<Path>) -> Result<Song, CodettaError> {
    let path = path.as_ref();
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(CodettaError::FileNotFound(path.to_path_buf()));
        }
        Err(e) => return Err(CodettaError::Io(e)),
    };
    let song: Song = serde_json::from_slice(&bytes)?;
    if !crate::SUPPORTED_VERSIONS.contains(&song.version.as_str()) {
        return Err(CodettaError::UnknownVersion(song.version));
    }
    Ok(song)
}

/// `.codetta` ファイルとして書き出す (pretty JSON, 末尾改行)。
pub fn save(song: &Song, path: impl AsRef<Path>, force: bool) -> Result<(), CodettaError> {
    let path = path.as_ref();
    if !force && path.exists() {
        return Err(CodettaError::FileExists(path.to_path_buf()));
    }
    let mut bytes = serde_json::to_vec_pretty(song)?;
    bytes.push(b'\n');
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Instrument, Note, Pitch, Track};

    fn sample_song() -> Song {
        let mut s = Song::new("Test", 140, Some("Am".into()));
        let mut inst = Instrument::new("soundfont");
        inst.params.insert(
            "file".into(),
            serde_json::json!("GeneralUser-GS-v1.471.sf2"),
        );
        inst.params.insert("preset".into(), serde_json::json!(81));
        inst.params.insert("bank".into(), serde_json::json!(0));
        s.tracks.push(Track {
            id: "lead".into(),
            name: "Lead".into(),
            instrument: inst,
            volume: 0.7,
            pan: 0.0,
            mute: false,
            solo: false,
            fx: vec![],
            notes: vec![Note {
                t: 0.0,
                pitch: Pitch::Name("A4".into()),
                dur: 0.5,
                vel: 100,
            }],
        });
        s
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = tempdir();
        let path = dir.join("song.codetta");
        let s = sample_song();
        save(&s, &path, false).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded, s);
    }

    #[test]
    fn save_refuses_to_overwrite_without_force() {
        let dir = tempdir();
        let path = dir.join("song.codetta");
        save(&sample_song(), &path, false).unwrap();
        let err = save(&sample_song(), &path, false).unwrap_err();
        assert!(matches!(err, CodettaError::FileExists(_)));
        // force なら通る
        save(&sample_song(), &path, true).unwrap();
    }

    #[test]
    fn load_missing_returns_file_not_found() {
        let dir = tempdir();
        let path = dir.join("nope.codetta");
        let err = load(&path).unwrap_err();
        assert!(matches!(err, CodettaError::FileNotFound(_)));
    }

    #[test]
    fn load_rejects_unknown_version() {
        let dir = tempdir();
        let path = dir.join("future.codetta");
        std::fs::write(
            &path,
            br#"{"version": "9.9", "metadata": {"name": "x", "bpm": 120}, "tracks": []}"#,
        )
        .unwrap();
        let err = load(&path).unwrap_err();
        assert!(matches!(err, CodettaError::UnknownVersion(v) if v == "9.9"));
    }

    fn tempdir() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("codetta-test-{nanos}-{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
