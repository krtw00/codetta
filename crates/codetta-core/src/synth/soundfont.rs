//! SoundFont (.sf2) ベースの sample render path。
//!
//! 設計詳細は docs/design/07-soundfont.md。
//! - `render_soundfont_note` — 1 ノート用 PoC ヘルパ (Phase 1)
//! - `SoundFontParams` / `resolve_soundfont_path` — params 解釈 + path 解決 (Phase 2)
//! - track 全体の render は `render/mod.rs` 側で `Synthesizer` を再利用する

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use serde_json::{Map, Value};
use thiserror::Error;

/// GM Drum (bank 128) の SF2 経路で扱う「要素名キー → MIDI 番号」 マップ。
///
/// LLM フレンドリーのため `pitch: "kick"` のような要素名で書ける糖衣表現を
/// SF2 経路 (= bank 128 track) で MIDI 番号に正規化する。
/// 02-project-format.md / 07-soundfont.md の正本。
pub const DRUM_KEY_MIDI_MAP: &[(&str, u8)] = &[
    ("kick", 36),
    ("snare", 38),
    ("clap", 39),
    ("tom_lo", 41),
    ("hh_closed", 42),
    ("hh_open", 46),
    ("tom_mid", 47),
    ("crash", 49),
    ("tom_hi", 50),
    ("ride", 51),
];

/// `DRUM_KEY_MIDI_MAP` のキーのみ。 validate / catalog で使う。
pub const KNOWN_DRUM_KEYS: &[&str] = &[
    "kick",
    "snare",
    "clap",
    "tom_lo",
    "hh_closed",
    "hh_open",
    "tom_mid",
    "crash",
    "tom_hi",
    "ride",
];

/// drum 要素名キーを GM Drum MIDI 番号に変換する。 未知キーは `None`。
pub fn drum_key_to_midi(key: &str) -> Option<u8> {
    DRUM_KEY_MIDI_MAP
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, n)| *n)
}

/// SF2 の drum bank (GM/GS 規約)。 bank 128 を drum kit として扱う。
pub const DRUM_BANK: u16 = 128;

/// `Instrument::SoundFont` の params (`file` / `preset` / `bank`)。
///
/// `from_params` で JSON Map から取り出し、 `validate` で各値の妥当性を検査する。
#[derive(Debug, Clone)]
pub struct SoundFontParams {
    pub file: String,
    pub preset: u16,
    pub bank: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundFontParamsError {
    InvalidFileType,
    EmptyFile,
    InvalidPreset(String),
    InvalidBank(String),
}

impl std::fmt::Display for SoundFontParamsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFileType => write!(f, "soundfont params.file must be a string"),
            Self::EmptyFile => write!(f, "soundfont params.file must be non-empty"),
            Self::InvalidPreset(s) => write!(f, "soundfont params.preset must be 0..=127, got {s}"),
            Self::InvalidBank(s) => write!(f, "soundfont params.bank must be 0..=128, got {s}"),
        }
    }
}

impl SoundFontParams {
    /// 既定の bank。 GM/GS 互換 SF2 で melodic 音色に使う。
    pub const DEFAULT_BANK: u16 = 0;
    /// 既定の preset。 GM Program 0 = Acoustic Grand Piano。
    pub const DEFAULT_PRESET: u16 = 0;

    pub fn from_params(params: &Map<String, Value>) -> Result<Self, SoundFontParamsError> {
        // `file` 省略時は bundle SF2 (= DEFAULT_SF2) にフォールバックする (CDT-12)。
        // 配布物では bundle SF2 が Release アーカイブ / Homebrew prefix に同梱され、
        // resolve_soundfont_path が解決する (= 09-distribution.md)。
        let file = match params.get("file") {
            None => crate::migrate::DEFAULT_SF2.to_string(),
            Some(v) => {
                let s = v.as_str().ok_or(SoundFontParamsError::InvalidFileType)?;
                if s.is_empty() {
                    return Err(SoundFontParamsError::EmptyFile);
                }
                s.to_string()
            }
        };

        let preset = match params.get("preset") {
            None => Self::DEFAULT_PRESET,
            Some(v) => {
                let n = v
                    .as_u64()
                    .ok_or_else(|| SoundFontParamsError::InvalidPreset(v.to_string()))?;
                if n > 127 {
                    return Err(SoundFontParamsError::InvalidPreset(n.to_string()));
                }
                n as u16
            }
        };

        let bank = match params.get("bank") {
            None => Self::DEFAULT_BANK,
            Some(v) => {
                let n = v
                    .as_u64()
                    .ok_or_else(|| SoundFontParamsError::InvalidBank(v.to_string()))?;
                if n > 128 {
                    return Err(SoundFontParamsError::InvalidBank(n.to_string()));
                }
                n as u16
            }
        };

        Ok(Self { file, preset, bank })
    }
}

/// SF2 path を解決する。 相対 path はユーザー指定 dir → bundle SF2 の順で探す (CDT-12)。
///
/// - 絶対 path: そのまま (env / bundle 無関係)
/// - 相対 path: 次の候補を順に探し、 **最初に存在するもの** を返す:
///   1. 主 path = `$CODETTA_SOUNDFONT_DIR/<file>` (env 設定時) または `$HOME/Music/sf2/<file>`
///   2. bundle SF2 ([`bundle_soundfont_dirs`] 参照) — Release アーカイブの `assets/`、
///      Homebrew prefix の `share/codetta/`
///
/// どれも存在しなければ主 path (= ユーザーが配置すべき場所) を返す。 これにより
/// `SOUNDFONT_FILE_NOT_FOUND` のメッセージが bundle 内部ではなくユーザー向けの場所を指す。
///
/// bundle SF2 はリポジトリに git-track せず、 配布物 (Release / Homebrew) にのみ同梱される
/// (= 09-distribution.md 決着済)。
pub fn resolve_soundfont_path(file: impl AsRef<Path>) -> PathBuf {
    let file = file.as_ref();
    if file.is_absolute() {
        return file.to_path_buf();
    }
    let primary = primary_soundfont_path(file);
    if primary.exists() {
        return primary;
    }
    for dir in bundle_soundfont_dirs() {
        let candidate = dir.join(file);
        if candidate.exists() {
            return candidate;
        }
    }
    primary
}

/// 相対 SF2 file の主 path (= ユーザーが配置する場所)。
/// `$CODETTA_SOUNDFONT_DIR` 設定時はそれ、 未設定なら `$HOME/Music/sf2/`、
/// `$HOME` も取れなければ相対 path をそのまま (CWD 基準扱い)。
fn primary_soundfont_path(file: &Path) -> PathBuf {
    if let Ok(dir) = std::env::var("CODETTA_SOUNDFONT_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join(file);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join("Music").join("sf2").join(file);
        }
    }
    file.to_path_buf()
}

/// bundle SF2 を探すディレクトリ候補 (= 配布物のレイアウト、 09-distribution.md)。
///
/// 実行バイナリの位置から導出する:
/// - GitHub Release アーカイブ (dist): `include` ファイルは archive ルート (= バイナリと同階層)
///   に平坦化されるため `<bin_dir>` 自身を最優先候補にする
/// - Homebrew: `<prefix>/share/codetta/`
/// - 念のため `<root>/assets/` (= bin/ の隣) と `<bin_dir>/assets/` も候補に残す
///
/// `current_exe` が取れない環境 (= 一部のサンドボックス) では空 Vec を返す。
fn bundle_soundfont_dirs() -> Vec<PathBuf> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let Some(bin_dir) = exe.parent() else {
        return Vec::new();
    };
    let mut dirs = vec![bin_dir.to_path_buf()];
    if let Some(root) = bin_dir.parent() {
        dirs.push(root.join("assets"));
        dirs.push(root.join("share").join("codetta"));
    }
    dirs.push(bin_dir.join("assets"));
    dirs
}

/// bundle SF2 (= GeneralUser GS v2.0.3) の自前再ホスト URL (= 専用 data Release、 09-distribution.md)。
/// tag は非バージョン形 (= release.yml の tag trigger に誤マッチしない)。
pub const BUNDLE_SF2_URL: &str =
    "https://github.com/krtw00/codetta/releases/download/soundfont-bundle/GeneralUser-GS.sf2";

/// bundle SF2 の固定 sha256 (= immutable asset)。 自動 DL 後にこの値で検証する。
pub const BUNDLE_SF2_SHA256: &str =
    "9575028c7a1f589f5770fccc8cff2734566af40cd26ed836944e9a5152688cfe";

#[derive(Debug, Error)]
pub enum BundleFetchError {
    #[error("curl コマンドが見つかりません (= bundle SoundFont の自動取得に必要)")]
    CurlMissing,
    #[error("bundle SoundFont の download に失敗しました: {0}")]
    Download(String),
    #[error("bundle SoundFont の sha256 が一致しません: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("bundle SoundFont の I/O エラー: {0}")]
    Io(#[from] std::io::Error),
}

/// bundle SF2 (= [`crate::migrate::DEFAULT_SF2`]) を主 path に用意する (CDT-13)。
///
/// cargo-dist の curl installer は include した SF2 を install しないため、 installer 経由では
/// SF2 がディスクに存在しない。 そこで render 時にこの関数で [`BUNDLE_SF2_URL`] から主 path
/// (`$CODETTA_SOUNDFONT_DIR` / `~/Music/sf2/`) へ自動 DL する (= 09-distribution.md)。
///
/// - 既に主 path に存在すれば即 `Ok` (= 再 DL しない、 何も出力しない)
/// - 無ければ curl で download し [`BUNDLE_SF2_SHA256`] で sha256 検証して配置する
/// - curl 不在 / network 失敗 / hash 不一致 は `Err` (= 呼び出し側が手動配置を案内)
///
/// DEFAULT_SF2 (= bundle SF2) 専用。 ユーザー指定の任意 SF2 名では呼ばない (= DL URL 不明)。
pub fn ensure_bundle_soundfont() -> Result<PathBuf, BundleFetchError> {
    let target = primary_soundfont_path(Path::new(crate::migrate::DEFAULT_SF2));
    if target.exists() {
        return Ok(target);
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    eprintln!(
        "[INFO] bundle SoundFont ({}) が見つかりません。 {} から取得します (約 30MB、 初回のみ)...",
        crate::migrate::DEFAULT_SF2,
        BUNDLE_SF2_URL
    );

    // 中断時に壊れた SF2 を残さないよう .part に落としてから atomic rename する。
    let tmp = target.with_file_name(format!(
        "{}.part",
        target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| crate::migrate::DEFAULT_SF2.to_string())
    ));
    let status = std::process::Command::new("curl")
        .args([
            "--proto",
            "=https",
            "--tlsv1.2",
            "-fSL",
            "--retry",
            "3",
            "-o",
        ])
        .arg(&tmp)
        .arg(BUNDLE_SF2_URL)
        .status()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BundleFetchError::CurlMissing
            } else {
                BundleFetchError::Io(e)
            }
        })?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(BundleFetchError::Download(format!(
            "curl exited with {status}"
        )));
    }

    let actual = sha256_file(&tmp)?;
    if !actual.eq_ignore_ascii_case(BUNDLE_SF2_SHA256) {
        let _ = std::fs::remove_file(&tmp);
        return Err(BundleFetchError::HashMismatch {
            expected: BUNDLE_SF2_SHA256.to_string(),
            actual,
        });
    }

    std::fs::rename(&tmp, &target)?;
    eprintln!(
        "[OK] bundle SoundFont を {} に配置しました",
        target.display()
    );
    Ok(target)
}

/// ファイルの sha256 を 16 進小文字文字列で返す。
fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[derive(Debug, Error)]
pub enum SoundFontError {
    #[error("SF2 file not found: {0}")]
    NotFound(PathBuf),

    #[error("I/O error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("SoundFont parse error ({path}): {message}")]
    Parse { path: PathBuf, message: String },

    #[error("Synthesizer init error: {0}")]
    Synth(String),
}

#[derive(Debug, Clone)]
pub struct SoundFontRenderParams {
    pub sf2_path: PathBuf,
    pub preset: u16,
    pub bank: u16,
    pub midi_key: u8,
    pub velocity: u8,
    pub hold_sec: f32,
    pub release_tail_sec: f32,
    pub sample_rate: u32,
}

#[derive(Debug, Clone)]
pub struct StereoBuffer {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

/// track 全体 (= 複数 note) を 1 つの `Synthesizer` で render するための入力。
///
/// `notes` は `start_sample` 昇順で渡す前提 (呼び出し側で sort 済み)。
/// `total_samples` は track 全体のサンプル長 (release tail まで含む)。
#[derive(Debug, Clone)]
pub struct SoundFontTrackRender {
    pub sf2_path: PathBuf,
    pub preset: u16,
    pub bank: u16,
    pub sample_rate: u32,
    pub total_samples: usize,
    pub notes: Vec<SoundFontTrackNote>,
}

#[derive(Debug, Clone, Copy)]
pub struct SoundFontTrackNote {
    /// note_on 発火時刻 (サンプル単位)
    pub start_sample: usize,
    /// note_off 発火時刻 (サンプル単位、 `start_sample` より大きい)
    pub end_sample: usize,
    pub midi_key: u8,
    pub velocity: u8,
}

/// SF2 track を 1 度の `Synthesizer` で render する。
///
/// rustysynth の `Synthesizer` は内部に channel/voice state を持つため per-voice 独立 closure
/// にはできない。 ここで note_on / note_off / render を時刻順に駆動し、 末尾の release tail も
/// `total_samples` まで render し切る。
///
/// 同一 SF2 を複数 track で参照する場合は `load_soundfont` で 1 度だけ load し
/// `render_soundfont_track_with` に `Arc<SoundFont>` を渡すと load を共有できる。
/// このトップレベル fn は単発 render 用に毎回 load する path を保持する。
pub fn render_soundfont_track(cfg: &SoundFontTrackRender) -> Result<StereoBuffer, SoundFontError> {
    let sound_font = load_soundfont(&cfg.sf2_path)?;
    render_soundfont_track_with(cfg, sound_font)
}

/// 既に load 済みの `Arc<SoundFont>` を使って track を render する。
///
/// `render_to_buffer` 側で同一 SF2 を track 跨ぎで共有するための entry point。
pub fn render_soundfont_track_with(
    cfg: &SoundFontTrackRender,
    sound_font: Arc<SoundFont>,
) -> Result<StereoBuffer, SoundFontError> {
    let mut synth = open_synth_with(sound_font, cfg.sample_rate, cfg.bank, cfg.preset)?;

    let channel: i32 = 0;
    let mut left = vec![0.0f32; cfg.total_samples];
    let mut right = vec![0.0f32; cfg.total_samples];

    // event = (sample_idx, on/off, key, velocity)。 単純に on / off を 2 イベントに展開し、
    // sample_idx 昇順 + 同時刻なら off → on の順 (= 同 sample で連続するノートが切れずに渡る)。
    enum Ev {
        Off { key: u8 },
        On { key: u8, vel: u8 },
    }
    let mut events: Vec<(usize, u8, Ev)> = Vec::with_capacity(cfg.notes.len() * 2);
    for n in &cfg.notes {
        events.push((
            n.start_sample,
            1,
            Ev::On {
                key: n.midi_key,
                vel: n.velocity,
            },
        ));
        events.push((n.end_sample, 0, Ev::Off { key: n.midi_key }));
    }
    events.sort_by_key(|(s, ord, _)| (*s, *ord));

    let mut cursor: usize = 0;
    for (sample_idx, _, ev) in events {
        let target = sample_idx.min(cfg.total_samples);
        if target > cursor {
            synth.render(&mut left[cursor..target], &mut right[cursor..target]);
            cursor = target;
        }
        match ev {
            Ev::On { key, vel } => synth.note_on(channel, key as i32, vel as i32),
            Ev::Off { key } => synth.note_off(channel, key as i32),
        }
    }

    if cursor < cfg.total_samples {
        synth.render(&mut left[cursor..], &mut right[cursor..]);
    }

    Ok(StereoBuffer { left, right })
}

/// SF2 ファイルを memory に load して `Arc<SoundFont>` として返す。
///
/// `Arc` で返すので、 複数の track / 複数の `Synthesizer` に同じ instance を渡して
/// load 重複を避けられる (`render_to_buffer` の SF2 cache が利用)。
pub fn load_soundfont(sf2_path: &Path) -> Result<Arc<SoundFont>, SoundFontError> {
    if !sf2_path.exists() {
        return Err(SoundFontError::NotFound(sf2_path.to_path_buf()));
    }

    let mut file = File::open(sf2_path).map_err(|e| SoundFontError::Io {
        path: sf2_path.to_path_buf(),
        source: e,
    })?;

    let sound_font = SoundFont::new(&mut file).map_err(|e| SoundFontError::Parse {
        path: sf2_path.to_path_buf(),
        message: format!("{:?}", e),
    })?;

    Ok(Arc::new(sound_font))
}

/// load 済みの `Arc<SoundFont>` から `Synthesizer` を初期化し、 bank select + program change を送る。
fn open_synth_with(
    sound_font: Arc<SoundFont>,
    sample_rate: u32,
    bank: u16,
    preset: u16,
) -> Result<Synthesizer, SoundFontError> {
    let settings = SynthesizerSettings::new(sample_rate as i32);
    let mut synth = Synthesizer::new(&sound_font, &settings)
        .map_err(|e| SoundFontError::Synth(format!("{:?}", e)))?;

    let channel: i32 = 0;
    // Bank select (CC0 = bank MSB, CC32 = LSB) + Program change。 GM/GS では bank 0 + program 0-127。
    synth.process_midi_message(channel, 0xB0, 0, bank as i32);
    synth.process_midi_message(channel, 0xB0, 32, 0);
    synth.process_midi_message(channel, 0xC0, preset as i32, 0);

    Ok(synth)
}

fn open_synth(
    sf2_path: &Path,
    sample_rate: u32,
    bank: u16,
    preset: u16,
) -> Result<Synthesizer, SoundFontError> {
    let sound_font = load_soundfont(sf2_path)?;
    open_synth_with(sound_font, sample_rate, bank, preset)
}

/// SF2 ファイルのメタ情報 (header chunk からの抜粋)。
///
/// rustysynth の [`rustysynth::SoundFontInfo`] を JSON で扱いやすい形に詰め直したもの。
/// LLM が「この SF2 は何者か」を素早く把握できるように、利用頻度の高い項目のみ含む。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoundFontMeta {
    pub bank_name: String,
    pub version: String,
    pub author: String,
    pub copyright: String,
    pub comments: String,
}

/// SF2 に含まれる 1 つの preset の bank / program 番号と名前。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetInfo {
    pub bank: u16,
    pub preset: u16,
    pub name: String,
}

/// SF2 ファイルから preset 一覧 + メタ情報を取り出す。
///
/// preset は (bank, preset) 昇順で返す (rustysynth 内部の順序は SF2 ファイルの記述順で
/// 安定しないため、LLM / UI 用途には sort 済みのほうが扱いやすい)。
///
/// 失敗パターン:
/// - file が存在しない → `SoundFontError::NotFound`
/// - 開けない / parse 失敗 → `SoundFontError::Io` / `SoundFontError::Parse`
pub fn list_soundfont_presets(
    sf2_path: impl AsRef<Path>,
) -> Result<(SoundFontMeta, Vec<PresetInfo>), SoundFontError> {
    let sf2_path = sf2_path.as_ref();
    if !sf2_path.exists() {
        return Err(SoundFontError::NotFound(sf2_path.to_path_buf()));
    }
    let mut file = File::open(sf2_path).map_err(|e| SoundFontError::Io {
        path: sf2_path.to_path_buf(),
        source: e,
    })?;
    let sound_font = SoundFont::new(&mut file).map_err(|e| SoundFontError::Parse {
        path: sf2_path.to_path_buf(),
        message: format!("{:?}", e),
    })?;

    let info = sound_font.get_info();
    let v = info.get_version();
    let meta = SoundFontMeta {
        bank_name: info.get_bank_name().to_string(),
        version: format!("{}.{}", v.get_major(), v.get_minor()),
        author: info.get_author().to_string(),
        copyright: info.get_copyright().to_string(),
        comments: info.get_comments().to_string(),
    };

    let mut presets: Vec<PresetInfo> = sound_font
        .get_presets()
        .iter()
        .map(|p| PresetInfo {
            bank: p.get_bank_number() as u16,
            preset: p.get_patch_number() as u16,
            name: p.get_name().to_string(),
        })
        .collect();
    presets.sort_by_key(|p| (p.bank, p.preset));

    Ok((meta, presets))
}

/// SF2 から 1 ノートを stereo PCM として render する PoC。
///
/// 呼び出し側で SF2 file path は絶対 path に解決済みである前提
/// (`$CODETTA_SOUNDFONT_DIR` の展開や `~` 展開は Phase 2 で render dispatch に組み込む)。
pub fn render_soundfont_note(
    params: &SoundFontRenderParams,
) -> Result<StereoBuffer, SoundFontError> {
    let mut synth = open_synth(
        &params.sf2_path,
        params.sample_rate,
        params.bank,
        params.preset,
    )?;
    let channel: i32 = 0;

    let hold_samples = (params.hold_sec * params.sample_rate as f32).round() as usize;
    let tail_samples = (params.release_tail_sec * params.sample_rate as f32).round() as usize;
    let total = hold_samples + tail_samples;

    let mut left = vec![0.0f32; total];
    let mut right = vec![0.0f32; total];

    synth.note_on(channel, params.midi_key as i32, params.velocity as i32);
    if hold_samples > 0 {
        synth.render(&mut left[..hold_samples], &mut right[..hold_samples]);
    }

    synth.note_off(channel, params.midi_key as i32);
    if tail_samples > 0 {
        synth.render(&mut left[hold_samples..], &mut right[hold_samples..]);
    }

    Ok(StereoBuffer { left, right })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sf2_from_env() -> Option<PathBuf> {
        std::env::var("CODETTA_TEST_SF2").ok().map(PathBuf::from)
    }

    #[test]
    fn drum_key_to_midi_all_known_keys() {
        // ADR (02-project-format.md / 07-soundfont.md) で定義された GM Drum マップを正本として
        // 全 10 entry が想定通り変換されるか確認。
        assert_eq!(drum_key_to_midi("kick"), Some(36));
        assert_eq!(drum_key_to_midi("snare"), Some(38));
        assert_eq!(drum_key_to_midi("clap"), Some(39));
        assert_eq!(drum_key_to_midi("tom_lo"), Some(41));
        assert_eq!(drum_key_to_midi("hh_closed"), Some(42));
        assert_eq!(drum_key_to_midi("hh_open"), Some(46));
        assert_eq!(drum_key_to_midi("tom_mid"), Some(47));
        assert_eq!(drum_key_to_midi("crash"), Some(49));
        assert_eq!(drum_key_to_midi("tom_hi"), Some(50));
        assert_eq!(drum_key_to_midi("ride"), Some(51));
    }

    #[test]
    fn drum_key_to_midi_unknown_returns_none() {
        assert_eq!(drum_key_to_midi("zap"), None);
        assert_eq!(drum_key_to_midi(""), None);
        assert_eq!(drum_key_to_midi("C4"), None);
    }

    #[test]
    fn drum_key_map_and_known_keys_are_aligned() {
        // `DRUM_KEY_MIDI_MAP` のキー集合と `KNOWN_DRUM_KEYS` が完全一致することを保証。
        let map_keys: Vec<&str> = DRUM_KEY_MIDI_MAP.iter().map(|(k, _)| *k).collect();
        assert_eq!(map_keys, KNOWN_DRUM_KEYS);
    }

    #[test]
    fn params_defaults_file_when_omitted() {
        // `file` 省略時は bundle SF2 (DEFAULT_SF2) にフォールバックする (CDT-12)。
        let m = Map::new();
        let p = SoundFontParams::from_params(&m).unwrap();
        assert_eq!(p.file, crate::migrate::DEFAULT_SF2);
        assert_eq!(p.preset, 0);
        assert_eq!(p.bank, 0);
    }

    #[test]
    fn params_rejects_non_string_file() {
        let mut m = Map::new();
        m.insert("file".into(), json!(123));
        assert_eq!(
            SoundFontParams::from_params(&m).unwrap_err(),
            SoundFontParamsError::InvalidFileType
        );
    }

    #[test]
    fn bundle_dirs_include_assets_candidate() {
        // current_exe が取れる test 環境では assets/ 候補を含む (= Release レイアウト)。
        let dirs = bundle_soundfont_dirs();
        assert!(
            dirs.iter().any(|d| d.ends_with("assets")),
            "expected an assets/ candidate, got: {dirs:?}"
        );
    }

    #[test]
    fn bundle_dirs_include_bin_dir_itself() {
        // dist は include ファイルを archive ルート (= バイナリ同階層) に平坦化するため、
        // bin_dir 自身が候補に含まれる必要がある (= 手動 tarball 解凍で file 省略が動く条件)。
        let exe = std::env::current_exe().unwrap();
        let bin_dir = exe.parent().unwrap();
        let dirs = bundle_soundfont_dirs();
        assert!(
            dirs.iter().any(|d| d == bin_dir),
            "expected bin_dir itself ({}) as a candidate, got: {dirs:?}",
            bin_dir.display()
        );
    }

    #[test]
    fn sha256_file_matches_known_vector() {
        // sha256("abc") の既知ベクトル。 自動 DL 後の検証ロジックが正しいことを保証する。
        let mut path = std::env::temp_dir();
        path.push(format!("codetta-sha256-test-{}.bin", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        let got = sha256_file(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(
            got,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn bundle_sf2_const_sha256_is_64_hex() {
        // 固定 hash が 64 文字の 16 進であることを保証 (= typo / 桁落ち検知)。
        assert_eq!(BUNDLE_SF2_SHA256.len(), 64);
        assert!(BUNDLE_SF2_SHA256.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn bundle_fetch_error_display() {
        assert_eq!(
            BundleFetchError::HashMismatch {
                expected: "aa".into(),
                actual: "bb".into(),
            }
            .to_string(),
            "bundle SoundFont の sha256 が一致しません: expected aa, got bb"
        );
        assert!(BundleFetchError::CurlMissing.to_string().contains("curl"));
    }

    #[test]
    fn params_rejects_empty_file() {
        let mut m = Map::new();
        m.insert("file".into(), json!(""));
        assert_eq!(
            SoundFontParams::from_params(&m).unwrap_err(),
            SoundFontParamsError::EmptyFile
        );
    }

    #[test]
    fn params_default_preset_bank() {
        let mut m = Map::new();
        m.insert("file".into(), json!("foo.sf2"));
        let p = SoundFontParams::from_params(&m).unwrap();
        assert_eq!(p.file, "foo.sf2");
        assert_eq!(p.preset, 0);
        assert_eq!(p.bank, 0);
    }

    #[test]
    fn params_preset_range_check() {
        let mut m = Map::new();
        m.insert("file".into(), json!("foo.sf2"));
        m.insert("preset".into(), json!(200));
        assert!(matches!(
            SoundFontParams::from_params(&m).unwrap_err(),
            SoundFontParamsError::InvalidPreset(_)
        ));
    }

    #[test]
    fn resolve_absolute_path_untouched() {
        let p = resolve_soundfont_path("/abs/path/x.sf2");
        assert_eq!(p, PathBuf::from("/abs/path/x.sf2"));
    }

    #[test]
    fn resolve_relative_uses_env() {
        let saved = std::env::var("CODETTA_SOUNDFONT_DIR").ok();
        std::env::set_var("CODETTA_SOUNDFONT_DIR", "/custom/sf2");
        let p = resolve_soundfont_path("piano.sf2");
        assert_eq!(p, PathBuf::from("/custom/sf2/piano.sf2"));
        match saved {
            Some(v) => std::env::set_var("CODETTA_SOUNDFONT_DIR", v),
            None => std::env::remove_var("CODETTA_SOUNDFONT_DIR"),
        }
    }

    #[test]
    fn missing_file_returns_not_found() {
        let params = SoundFontRenderParams {
            sf2_path: PathBuf::from("/nonexistent/codetta-test/missing.sf2"),
            preset: 0,
            bank: 0,
            midi_key: 60,
            velocity: 100,
            hold_sec: 0.1,
            release_tail_sec: 0.1,
            sample_rate: 44100,
        };
        let err = render_soundfont_note(&params).unwrap_err();
        assert!(matches!(err, SoundFontError::NotFound(_)));
    }

    #[test]
    fn renders_c4_when_sf2_available() {
        let Some(sf2) = sf2_from_env() else {
            eprintln!("CODETTA_TEST_SF2 not set — skipping (PoC SF2 render test)");
            return;
        };

        let params = SoundFontRenderParams {
            sf2_path: sf2,
            preset: 0, // GM Program 0 = Acoustic Grand Piano
            bank: 0,
            midi_key: 60, // C4
            velocity: 100,
            hold_sec: 1.0,
            release_tail_sec: 0.5,
            sample_rate: 44100,
        };

        let buf = render_soundfont_note(&params).expect("SF2 render should succeed");

        let expected_samples = 44100 + 22050;
        assert_eq!(buf.left.len(), expected_samples);
        assert_eq!(buf.right.len(), expected_samples);

        let peak = buf
            .left
            .iter()
            .chain(buf.right.iter())
            .fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(peak > 0.01, "rendered peak suspiciously low: {peak}");
        assert!(peak <= 2.0, "rendered peak suspiciously high: {peak}");

        let hold_peak = buf
            .left
            .iter()
            .take(44100)
            .fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(
            hold_peak > 0.01,
            "no signal during hold phase: peak={hold_peak}"
        );
    }

    #[test]
    fn list_presets_returns_not_found_for_missing_file() {
        let err = list_soundfont_presets("/nonexistent/codetta-test/missing.sf2").unwrap_err();
        assert!(matches!(err, SoundFontError::NotFound(_)));
    }

    #[test]
    fn list_presets_when_sf2_available() {
        let Some(sf2) = sf2_from_env() else {
            eprintln!("CODETTA_TEST_SF2 not set — skipping SF2 preset enumeration test");
            return;
        };

        let (meta, presets) =
            list_soundfont_presets(&sf2).expect("SF2 preset enumeration should succeed");

        // GeneralUser-GS は 1 つは preset を持つ
        assert!(
            !presets.is_empty(),
            "expected at least one preset in {}",
            sf2.display()
        );

        // GM 互換 SF2 なら bank 0 / preset 0 が「Acoustic Grand Piano」相当で必ず入る
        let gm_piano = presets.iter().find(|p| p.bank == 0 && p.preset == 0);
        assert!(
            gm_piano.is_some(),
            "expected (bank=0, preset=0) entry in GM/GS-compatible SF2"
        );

        // sort 済みであることを確認 (bank → preset 昇順)
        for w in presets.windows(2) {
            let a = (w[0].bank, w[0].preset);
            let b = (w[1].bank, w[1].preset);
            assert!(
                a <= b,
                "presets should be sorted by (bank, preset): {:?} <= {:?}",
                a,
                b
            );
        }

        // meta の bank_name は空でないことが多いが、保証はないので存在チェックのみ。
        // version は SF2 仕様で 16bit major/minor → 文字列表現が "X.Y" 形式になる
        assert!(
            meta.version.contains('.'),
            "version should be major.minor: {}",
            meta.version
        );
    }

    #[test]
    fn load_soundfont_returns_not_found_for_missing_file() {
        let err = load_soundfont(Path::new("/nonexistent/codetta-test/missing.sf2")).unwrap_err();
        assert!(matches!(err, SoundFontError::NotFound(_)));
    }

    #[test]
    fn load_soundfont_arc_is_shareable_when_sf2_available() {
        // 同一 SF2 を 2 回 load せず、 Arc::clone で複数の render_soundfont_track_with に
        // 渡せることを確認する (render/mod.rs の SF2 cache が期待する API 形)。
        let Some(sf2) = sf2_from_env() else {
            eprintln!("CODETTA_TEST_SF2 not set — skipping load_soundfont Arc share test");
            return;
        };
        let sf = load_soundfont(&sf2).expect("SF2 load should succeed");

        // Arc::clone で 2 つの track をそれぞれ render しても落ちず、 同じ仕様なら同じ波形になる。
        let sr = 44100u32;
        let total = (sr as f32 * 1.0) as usize;
        let notes = vec![SoundFontTrackNote {
            start_sample: 0,
            end_sample: (sr as f32 * 0.5) as usize,
            midi_key: 60,
            velocity: 100,
        }];
        let cfg = SoundFontTrackRender {
            sf2_path: sf2.clone(),
            preset: 0,
            bank: 0,
            sample_rate: sr,
            total_samples: total,
            notes,
        };
        let a = render_soundfont_track_with(&cfg, sf.clone()).expect("render a");
        let b = render_soundfont_track_with(&cfg, sf.clone()).expect("render b");
        assert_eq!(a.left.len(), b.left.len());
        // 同じ SF2 / 同じ cfg → 完全に同一の output になる (deterministic)
        let max_diff = a
            .left
            .iter()
            .zip(b.left.iter())
            .map(|(x, y)| (x - y).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            max_diff < 1e-6,
            "shared Arc<SoundFont> should produce identical output: max_diff={max_diff}"
        );
    }

    #[test]
    fn track_renders_multiple_notes_when_sf2_available() {
        let Some(sf2) = sf2_from_env() else {
            eprintln!("CODETTA_TEST_SF2 not set — skipping SF2 track render test");
            return;
        };

        // 0.0s / 0.5s / 1.0s に C4 / E4 / G4 を打鍵 (各 0.4s ホールド)
        let sr = 44100u32;
        let total = (sr as f32 * 2.0) as usize;
        let notes = vec![
            SoundFontTrackNote {
                start_sample: 0,
                end_sample: (sr as f32 * 0.4) as usize,
                midi_key: 60,
                velocity: 100,
            },
            SoundFontTrackNote {
                start_sample: (sr as f32 * 0.5) as usize,
                end_sample: (sr as f32 * 0.9) as usize,
                midi_key: 64,
                velocity: 100,
            },
            SoundFontTrackNote {
                start_sample: (sr as f32 * 1.0) as usize,
                end_sample: (sr as f32 * 1.4) as usize,
                midi_key: 67,
                velocity: 100,
            },
        ];
        let cfg = SoundFontTrackRender {
            sf2_path: sf2,
            preset: 0,
            bank: 0,
            sample_rate: sr,
            total_samples: total,
            notes,
        };
        let buf = render_soundfont_track(&cfg).expect("SF2 track render");
        assert_eq!(buf.left.len(), total);
        assert_eq!(buf.right.len(), total);
        let peak = buf
            .left
            .iter()
            .chain(buf.right.iter())
            .fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(peak > 0.01, "track peak too low: {peak}");
    }
}
