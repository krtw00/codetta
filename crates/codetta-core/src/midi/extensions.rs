//! MIDI に埋め込まれた codetta 拡張属性 (master_gain / fx / SF2 preset 詳細) の復元。
//!
//! 設計: docs/design/08-midi.md「schema 拡張属性の round-trip」

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::midi::error::MidiError;

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
