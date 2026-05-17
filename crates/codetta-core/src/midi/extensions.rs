//! MIDI に埋め込まれた codetta 拡張属性 (master_gain / fx / SF2 preset 詳細) の復元。
//!
//! 設計: docs/design/08-midi.md「schema 拡張属性の round-trip」

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::midi::error::MidiError;
use crate::model::Song;

/// `--extensions` モード (CLI / MCP / lib 共通)。
///
/// - `TextMeta` (default): MTrk 0 先頭の Text Meta Event に JSON inline
/// - `Sidecar`: `.mid` 横の `<basename>.codetta.meta.json`
/// - `None`: 拡張属性復元なし (純粋 GM 互換)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionsMode {
    TextMeta,
    Sidecar,
    None,
}

impl ExtensionsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TextMeta => "text-meta",
            Self::Sidecar => "sidecar",
            Self::None => "none",
        }
    }
}

/// text-meta / sidecar から読み込まれる JSON shape (ADR 8 章)。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct ExtensionsPayload {
    pub codetta: ExtensionsRoot,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct ExtensionsRoot {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub metadata: Option<ExtensionsMetadata>,
    #[serde(default)]
    pub tracks: Vec<ExtensionsTrack>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct ExtensionsMetadata {
    #[serde(default)]
    pub master_gain: Option<f32>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct ExtensionsTrack {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub instrument: Option<Value>,
    #[serde(default)]
    pub fx: Option<Value>,
}

/// codetta 拡張 Text Meta payload のプレフィックス。
/// `MTrk` 0 先頭で最初に見つかった Text Meta Event (`FF 01`) のうち、
/// この prefix で始まるものを採用する (= 他ツールが書いた generic な Text Meta との区別)。
pub(crate) const TEXT_META_PREFIX: &str = "{\"codetta\":";

/// 与えられた `.mid` path から sidecar JSON の path を導出する
/// (= `<basename>.codetta.meta.json`、 ADR L411)。
pub(crate) fn sidecar_path_for(mid_path: &Path) -> PathBuf {
    let mut sidecar = mid_path.to_path_buf();
    sidecar.set_extension("codetta.meta.json");
    sidecar
}

/// Text Meta Event の bytes から codetta JSON object を取り出す。
///
/// `TEXT_META_PREFIX` で始まるもののみ採用 (= 他ツールが書いた generic Text Meta は無視)。
pub(crate) fn parse_text_meta(bytes: &[u8]) -> Option<Result<ExtensionsPayload, MidiError>> {
    let s = std::str::from_utf8(bytes).ok()?;
    let trimmed = s.trim_start();
    if !trimmed.starts_with(TEXT_META_PREFIX) {
        return None;
    }
    Some(
        serde_json::from_str::<ExtensionsPayload>(trimmed)
            .map_err(|e| MidiError::InvalidExtensions(e.to_string())),
    )
}

/// sidecar JSON ファイルを読み込んで payload に変換する。
/// 存在しなければ `Ok(None)`、 壊れていれば `Err`。
pub(crate) fn load_sidecar(path: &Path) -> Result<Option<ExtensionsPayload>, MidiError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(MidiError::Io(e)),
    };
    serde_json::from_slice::<ExtensionsPayload>(&bytes)
        .map(Some)
        .map_err(|e| MidiError::InvalidExtensions(e.to_string()))
}

/// Song から MIDI 拡張属性 payload を構築する (ADR L147-L183 の JSON shape)。
///
/// - `metadata`: master_gain (default 1.0 と一致しても常に書く: round-trip 一貫性のため)、
///   key (Option)、 tags (空でなければ)
/// - `tracks[]`: id / name / instrument / fx を書く。 並びは [`tracks_in_channel_order`] と同じ
///   = channel index 昇順 (drum はその位置に挟まる)。
/// - notes / volume / pan / mute / solo / bpm / time_signature は MIDI 側で表現するので含めない
///   (ADR L192-L197)。
pub(crate) fn build_text_meta_payload(song: &Song) -> ExtensionsPayload {
    let metadata = ExtensionsMetadata {
        master_gain: Some(song.metadata.master_gain),
        key: song.metadata.key.clone(),
        tags: song.metadata.tags.clone(),
    };

    let tracks_in_order = tracks_in_channel_order(song);
    let tracks: Vec<ExtensionsTrack> = tracks_in_order
        .iter()
        .map(|t| {
            let instrument = serde_json::to_value(&t.instrument).ok();
            // fx 配列は serde で Vec<Effect> として再構成可。 空配列でも明示的に書いて
            // import 側の「override あり」判定を確定的にする。
            let fx = serde_json::to_value(&t.fx).ok();
            ExtensionsTrack {
                id: Some(t.id.clone()),
                name: Some(t.name.clone()),
                instrument,
                fx,
            }
        })
        .collect();

    ExtensionsPayload {
        codetta: ExtensionsRoot {
            version: Some(crate::SCHEMA_VERSION.to_string()),
            metadata: Some(metadata),
            tracks,
        },
    }
}

/// payload を Text Meta Event 用の UTF-8 bytes に serialize する (改行なし、 単一行)。
pub(crate) fn payload_to_text_meta_bytes(payload: &ExtensionsPayload) -> Vec<u8> {
    // serde_json::to_vec は単一行、 ASCII 範囲。 ADR L143 (= 改行なし)。
    serde_json::to_vec(payload).expect("ExtensionsPayload is always serializable")
}

/// sidecar JSON を書き出す (`<basename>.codetta.meta.json`)。
/// 形式は text-meta と同一 (= `{ "codetta": { ... } }`)、 pretty 出力で人間が読みやすい形にする。
pub(crate) fn save_sidecar(path: &Path, payload: &ExtensionsPayload) -> Result<(), MidiError> {
    let mut bytes = serde_json::to_vec_pretty(payload)
        .map_err(|e| MidiError::InvalidExtensions(e.to_string()))?;
    bytes.push(b'\n');
    std::fs::write(path, &bytes).map_err(MidiError::Io)
}

/// MIDI export の channel 割当順で `Song.tracks` を並べた参照列を返す。
///
/// ADR L60-L67 の規約に従う:
/// - drum (= `instrument.params.bank == 128`) は ch10 (= idx 9) 固定
/// - melodic は出現順に ch1, ch2, ..., ch9, ch11, ch12, ..., ch16
///
/// この関数は **割当順** に並べた `Vec<&Track>` を返すだけで、 channel 上限超過 (= 16 melodic 以上)
/// や複数 drum の検出はしない (= caller = export 側で error を出す)。
pub(crate) fn tracks_in_channel_order(song: &Song) -> Vec<&crate::model::Track> {
    let mut melodic: Vec<&crate::model::Track> = Vec::new();
    let mut drum: Option<&crate::model::Track> = None;
    for t in &song.tracks {
        if track_is_drum(t) {
            // 複数あった場合は最後の 1 本を採用 (= 検証は export 側で別途)
            drum = Some(t);
        } else {
            melodic.push(t);
        }
    }
    // ADR L60: ch1, ch2, ..., ch9, [drum=ch10], ch11, ..., ch16 の順で並ぶ。
    // melodic は出現順、 ch10 の位置に drum を挟む (= 9 番目)。
    let mut out: Vec<&crate::model::Track> = Vec::with_capacity(song.tracks.len());
    for (i, t) in melodic.into_iter().enumerate() {
        if i == 9 {
            if let Some(d) = drum.take() {
                out.push(d);
            }
        }
        out.push(t);
    }
    // melodic が 9 未満で終わった場合 (= drum が後ろに残る) は末尾に追加
    if let Some(d) = drum {
        out.push(d);
    }
    out
}

/// `instrument.params.bank == 128` を持つ track を drum とみなす。
/// bank が無い / 数値でない場合は melodic 扱い。
pub(crate) fn track_is_drum(track: &crate::model::Track) -> bool {
    track
        .instrument
        .params
        .get("bank")
        .and_then(|v| v.as_u64())
        .map(|n| n == 128)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_path_replaces_extension() {
        assert_eq!(
            sidecar_path_for(Path::new("/tmp/foo.mid")),
            PathBuf::from("/tmp/foo.codetta.meta.json")
        );
    }

    #[test]
    fn parse_text_meta_accepts_codetta_payload() {
        let s = br#"{"codetta":{"version":"0.2","tracks":[]}}"#;
        let parsed = parse_text_meta(s).expect("recognized").expect("valid");
        assert_eq!(parsed.codetta.version.as_deref(), Some("0.2"));
    }

    #[test]
    fn parse_text_meta_ignores_non_codetta_text() {
        assert!(parse_text_meta(b"track name etc.").is_none());
        assert!(parse_text_meta(b"{\"other\":1}").is_none());
    }

    #[test]
    fn parse_text_meta_reports_invalid_json() {
        let err = parse_text_meta(b"{\"codetta\":{bogus}}")
            .expect("recognized")
            .unwrap_err();
        assert!(matches!(err, MidiError::InvalidExtensions(_)));
    }
}
