//! Codetta CLI entry point.
//!
//! 設計: docs/design/03-cli.md
//!
//! Phase 0 現スコープ: `new` / `info` / `validate` / `render` + track/notes 編集系。
//! 残コマンド (`set-instrument` / `set-fx` / `edit-notes` / `schema` 他) は続く実装で追加する。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use codetta_core::{self as core, CodettaError, Effect, Instrument, Note, NoteOp, Song, Track};
use serde_json::{json, Map, Value};

#[derive(Parser)]
#[command(
    name = "codetta",
    version,
    about = "Codetta — AI 作曲ツール / DAW-like CLI",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(flatten)]
    common: CommonOpts,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct CommonOpts {
    /// stderr の人間向けログを抑制
    #[arg(short, long, global = true)]
    quiet: bool,
    /// stderr に詳細ログを出力 (Phase 0 では quiet との on/off のみ)
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Command {
    /// 新規プロジェクトファイルを作成
    New(NewArgs),
    /// プロジェクトファイルのメタ情報を JSON 出力
    Info(InfoArgs),
    /// スキーマ + 整合性検証
    Validate(ValidateArgs),
    /// WAV ファイルにレンダリング
    Render(RenderArgs),
    /// トラックを追加
    AddTrack(AddTrackArgs),
    /// トラックを削除
    RemoveTrack(RemoveTrackArgs),
    /// トラックのノート列を全置換
    SetNotes(SetNotesArgs),
    /// トラックにノートを追加
    AddNotes(AddNotesArgs),
    /// トラックのノートを全削除
    ClearNotes(ClearNotesArgs),
    /// トラックの楽器を変更
    SetInstrument(SetInstrumentArgs),
    /// トラックのエフェクトチェーンを全置換
    SetFx(SetFxArgs),
    /// ノートに対する一括変形 (transpose / shift / scale / quantize 等)
    EditNotes(EditNotesArgs),
}

#[derive(Args)]
struct NewArgs {
    /// 出力ファイルパス (`.codetta` 推奨)
    path: PathBuf,
    /// 1 分間の拍数 (20-300)
    #[arg(long, default_value_t = 120)]
    bpm: u32,
    /// 調 (例: "Am", "C", "F#m")
    #[arg(long)]
    key: Option<String>,
    /// 楽曲名 (省略時はファイル名 stem)
    #[arg(long)]
    name: Option<String>,
    /// 拍子 N/D (例: "4/4")
    #[arg(long = "time-sig", value_parser = parse_time_sig, default_value = "4/4")]
    time_sig: [u32; 2],
    /// 既存ファイルを上書き
    #[arg(long)]
    force: bool,
}

fn parse_time_sig(s: &str) -> Result<[u32; 2], String> {
    let (n, d) = s
        .split_once('/')
        .ok_or_else(|| "expected N/D (e.g. 4/4)".to_string())?;
    let n: u32 = n.parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
    let d: u32 = d.parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
    Ok([n, d])
}

#[derive(Args)]
struct InfoArgs {
    path: PathBuf,
}

#[derive(Args)]
struct ValidateArgs {
    path: PathBuf,
}

#[derive(Args)]
struct AddTrackArgs {
    /// プロジェクトファイル
    path: PathBuf,
    /// トラック ID (kebab-case 推奨、 必須)
    #[arg(long)]
    id: String,
    /// 表示名 (省略時は id と同じ)
    #[arg(long)]
    name: Option<String>,
    /// 楽器 type (デフォルト `sin`)
    #[arg(long, default_value = "sin")]
    instrument: String,
    /// 音量 0.0-1.0
    #[arg(long, default_value_t = 0.8)]
    volume: f32,
    /// パン -1.0 (L) 〜 1.0 (R)
    #[arg(long, default_value_t = 0.0)]
    pan: f32,
    /// 楽器 params の JSON オブジェクト (例: `'{"attack":0.02}'`)
    #[arg(long = "params-json")]
    params_json: Option<String>,
}

#[derive(Args)]
struct RemoveTrackArgs {
    path: PathBuf,
    /// 削除対象トラック ID
    #[arg(long)]
    id: String,
}

#[derive(Args)]
struct SetNotesArgs {
    path: PathBuf,
    /// 対象トラック ID
    #[arg(long)]
    track: String,
    /// ノート列を JSON 文字列で渡す (排他: `--notes-file`)
    #[arg(long = "notes-json", conflicts_with = "notes_file")]
    notes_json: Option<String>,
    /// ノート列を JSON ファイルで渡す
    #[arg(long = "notes-file", conflicts_with = "notes_json")]
    notes_file: Option<PathBuf>,
}

#[derive(Args)]
struct AddNotesArgs {
    path: PathBuf,
    #[arg(long)]
    track: String,
    #[arg(long = "notes-json", conflicts_with = "notes_file")]
    notes_json: Option<String>,
    #[arg(long = "notes-file", conflicts_with = "notes_json")]
    notes_file: Option<PathBuf>,
}

#[derive(Args)]
struct ClearNotesArgs {
    path: PathBuf,
    #[arg(long)]
    track: String,
}

#[derive(Args)]
struct SetInstrumentArgs {
    path: PathBuf,
    /// 対象トラック ID
    #[arg(long)]
    track: String,
    /// 楽器 type (必須)
    #[arg(long = "type")]
    kind: String,
    /// 楽器 params の JSON オブジェクト (省略時は空)
    #[arg(long = "params-json")]
    params_json: Option<String>,
}

#[derive(Args)]
struct EditNotesArgs {
    path: PathBuf,
    /// 対象トラック ID
    #[arg(long)]
    track: String,
    /// 操作配列を JSON 文字列で渡す (排他: `--ops-file`)。
    #[arg(long = "ops-json", conflicts_with = "ops_file")]
    ops_json: Option<String>,
    /// 操作配列を JSON ファイルで渡す。
    #[arg(long = "ops-file", conflicts_with = "ops_json")]
    ops_file: Option<PathBuf>,
}

#[derive(Args)]
struct SetFxArgs {
    path: PathBuf,
    #[arg(long)]
    track: String,
    /// fx チェーンを JSON 配列で渡す (排他: `--fx-file`)。
    /// 各要素は `{"type":"<name>", ...params}` 形式。
    #[arg(long = "fx-json", conflicts_with = "fx_file")]
    fx_json: Option<String>,
    #[arg(long = "fx-file", conflicts_with = "fx_json")]
    fx_file: Option<PathBuf>,
}

#[derive(Args)]
struct RenderArgs {
    /// 入力 `.codetta` ファイル
    path: PathBuf,
    /// 出力 WAV ファイルパス
    #[arg(short, long)]
    output: PathBuf,
    /// サンプルレート (Phase 0 first cut は 44100 のみ)
    #[arg(long, default_value_t = 44100)]
    sample_rate: u32,
    /// ビット深度 (Phase 0 first cut は 16 のみ)
    #[arg(long, default_value_t = 16)]
    bit_depth: u16,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let exit = match cli.command {
        Command::New(a) => cmd_new(a, &cli.common),
        Command::Info(a) => cmd_info(a, &cli.common),
        Command::Validate(a) => cmd_validate(a, &cli.common),
        Command::Render(a) => cmd_render(a, &cli.common),
        Command::AddTrack(a) => cmd_add_track(a, &cli.common),
        Command::RemoveTrack(a) => cmd_remove_track(a, &cli.common),
        Command::SetNotes(a) => cmd_set_notes(a, &cli.common),
        Command::AddNotes(a) => cmd_add_notes(a, &cli.common),
        Command::ClearNotes(a) => cmd_clear_notes(a, &cli.common),
        Command::SetInstrument(a) => cmd_set_instrument(a, &cli.common),
        Command::SetFx(a) => cmd_set_fx(a, &cli.common),
        Command::EditNotes(a) => cmd_edit_notes(a, &cli.common),
    };
    ExitCode::from(exit)
}

fn cmd_new(args: NewArgs, common: &CommonOpts) -> u8 {
    let name = args.name.unwrap_or_else(|| {
        args.path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_string()
    });

    let mut song = core::Song::new(name, args.bpm, args.key);
    song.metadata.time_signature = args.time_sig;

    if !common.quiet {
        eprintln!("[INFO] Creating {}", args.path.display());
    }

    if let Err(e) = core::save(&song, &args.path, args.force) {
        return emit_error(&e);
    }

    let abs = std::fs::canonicalize(&args.path).unwrap_or_else(|_| args.path.clone());
    emit_json(&json!({
        "ok": true,
        "path": abs.to_string_lossy(),
        "version": core::SCHEMA_VERSION,
    }));
    0
}

fn cmd_info(args: InfoArgs, common: &CommonOpts) -> u8 {
    let song = match core::load(&args.path) {
        Ok(s) => s,
        Err(e) => return emit_error(&e),
    };
    if !common.quiet {
        eprintln!("[INFO] Loaded {}", args.path.display());
    }

    let tracks: Vec<Value> = song
        .tracks
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "name": t.name,
                "instrument": t.instrument.kind,
                "note_count": t.notes.len(),
                "fx_count": t.fx.len(),
            })
        })
        .collect();

    emit_json(&json!({
        "ok": true,
        "version": song.version,
        "metadata": song.metadata,
        "tracks": tracks,
        "duration_beats": song.duration_beats(),
        "duration_sec": song.duration_sec(),
    }));
    0
}

fn cmd_validate(args: ValidateArgs, common: &CommonOpts) -> u8 {
    let song = match core::load(&args.path) {
        Ok(s) => s,
        Err(e) => return emit_error(&e),
    };
    let errors = core::validate(&song);
    if errors.is_empty() {
        if !common.quiet {
            eprintln!("[OK] {} is valid", args.path.display());
        }
        emit_json(&json!({ "ok": true }));
        0
    } else {
        if !common.quiet {
            eprintln!("[ERROR] {} validation error(s)", errors.len());
        }
        emit_json(&json!({ "ok": false, "errors": errors }));
        1
    }
}

fn cmd_render(args: RenderArgs, common: &CommonOpts) -> u8 {
    // Phase 0 first cut の制約: sample_rate / bit_depth は固定値のみ
    if args.sample_rate != 44100 {
        emit_json(&json!({
            "ok": false,
            "errors": [{
                "code": "RENDER_FAILED",
                "message": format!("sample_rate {} not supported (Phase 0 first cut: 44100 only)", args.sample_rate),
            }]
        }));
        return 1;
    }
    if args.bit_depth != 16 {
        emit_json(&json!({
            "ok": false,
            "errors": [{
                "code": "RENDER_FAILED",
                "message": format!("bit_depth {} not supported (Phase 0 first cut: 16 only)", args.bit_depth),
            }]
        }));
        return 1;
    }

    let song = match core::load(&args.path) {
        Ok(s) => s,
        Err(e) => return emit_error(&e),
    };
    let verrs = core::validate(&song);
    if !verrs.is_empty() {
        if !common.quiet {
            eprintln!("[ERROR] {} validation error(s)", verrs.len());
        }
        emit_json(&json!({ "ok": false, "errors": verrs }));
        return 1;
    }

    if !common.quiet {
        eprintln!("[INFO] Rendering {} → {}", args.path.display(), args.output.display());
    }

    let t0 = std::time::Instant::now();
    let stats = match core::render_to_wav(&song, &args.output) {
        Ok(s) => s,
        Err(e) => return emit_error(&e),
    };
    let elapsed = t0.elapsed().as_secs_f32();
    let rtfactor = if elapsed > 0.0 { stats.duration_sec / elapsed } else { 0.0 };

    if !common.quiet {
        eprintln!(
            "[OK] Wrote {} ({:.2}s @ {}Hz, {}bit) in {:.2}s [{:.1}x realtime]",
            args.output.display(),
            stats.duration_sec,
            stats.sample_rate,
            stats.bit_depth,
            elapsed,
            rtfactor,
        );
    }

    let abs = std::fs::canonicalize(&args.output).unwrap_or_else(|_| args.output.clone());
    emit_json(&json!({
        "ok": true,
        "output": abs.to_string_lossy(),
        "duration_sec": stats.duration_sec,
        "sample_rate": stats.sample_rate,
        "bit_depth": stats.bit_depth,
        "render_time_sec": elapsed,
        "rtfactor": rtfactor,
    }));
    0
}

fn cmd_add_track(args: AddTrackArgs, common: &CommonOpts) -> u8 {
    let mut song = match core::load(&args.path) {
        Ok(s) => s,
        Err(e) => return emit_error(&e),
    };

    let params = match args.params_json.as_deref() {
        None => Map::new(),
        Some(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Object(m)) => m,
            Ok(_) => {
                emit_json(&json!({
                    "ok": false,
                    "errors": [{ "code": "INVALID_JSON", "message": "--params-json must be a JSON object" }]
                }));
                return 1;
            }
            Err(e) => return emit_error(&CodettaError::InvalidJson(e)),
        },
    };

    let track = Track {
        id: args.id.clone(),
        name: args.name.unwrap_or_else(|| args.id.clone()),
        instrument: Instrument {
            kind: args.instrument,
            params,
        },
        volume: args.volume,
        pan: args.pan,
        mute: false,
        solo: false,
        fx: vec![],
        notes: vec![],
    };

    if let Err(e) = core::add_track(&mut song, track) {
        return emit_error(&e);
    }
    if let Some(code) = save_after_validate(&song, &args.path, common) {
        return code;
    }

    if !common.quiet {
        eprintln!("[OK] Added track '{}'", args.id);
    }
    emit_json(&json!({ "ok": true, "track_id": args.id }));
    0
}

fn cmd_remove_track(args: RemoveTrackArgs, common: &CommonOpts) -> u8 {
    let mut song = match core::load(&args.path) {
        Ok(s) => s,
        Err(e) => return emit_error(&e),
    };
    if let Err(e) = core::remove_track(&mut song, &args.id) {
        return emit_error(&e);
    }
    if let Some(code) = save_after_validate(&song, &args.path, common) {
        return code;
    }
    if !common.quiet {
        eprintln!("[OK] Removed track '{}'", args.id);
    }
    emit_json(&json!({ "ok": true, "track_id": args.id }));
    0
}

fn cmd_set_notes(args: SetNotesArgs, common: &CommonOpts) -> u8 {
    let mut song = match core::load(&args.path) {
        Ok(s) => s,
        Err(e) => return emit_error(&e),
    };
    let notes = match load_notes(args.notes_json.as_deref(), args.notes_file.as_deref()) {
        Ok(n) => n,
        Err(code) => return code,
    };
    let count = match core::set_notes(&mut song, &args.track, notes) {
        Ok(n) => n,
        Err(e) => return emit_error(&e),
    };
    if let Some(code) = save_after_validate(&song, &args.path, common) {
        return code;
    }
    if !common.quiet {
        eprintln!("[OK] Set {count} note(s) on track '{}'", args.track);
    }
    emit_json(&json!({ "ok": true, "track_id": args.track, "note_count": count }));
    0
}

fn cmd_add_notes(args: AddNotesArgs, common: &CommonOpts) -> u8 {
    let mut song = match core::load(&args.path) {
        Ok(s) => s,
        Err(e) => return emit_error(&e),
    };
    let notes = match load_notes(args.notes_json.as_deref(), args.notes_file.as_deref()) {
        Ok(n) => n,
        Err(code) => return code,
    };
    let (added, skipped, total) = match core::add_notes(&mut song, &args.track, notes) {
        Ok(t) => t,
        Err(e) => return emit_error(&e),
    };
    if let Some(code) = save_after_validate(&song, &args.path, common) {
        return code;
    }
    if !common.quiet {
        eprintln!(
            "[OK] Added {added} note(s) to '{}' (skipped {skipped} dup, total {total})",
            args.track
        );
    }
    emit_json(&json!({
        "ok": true,
        "track_id": args.track,
        "added": added,
        "skipped_duplicates": skipped,
        "total_notes": total,
    }));
    0
}

fn cmd_clear_notes(args: ClearNotesArgs, common: &CommonOpts) -> u8 {
    let mut song = match core::load(&args.path) {
        Ok(s) => s,
        Err(e) => return emit_error(&e),
    };
    let removed = match core::clear_notes(&mut song, &args.track) {
        Ok(n) => n,
        Err(e) => return emit_error(&e),
    };
    if let Some(code) = save_after_validate(&song, &args.path, common) {
        return code;
    }
    if !common.quiet {
        eprintln!("[OK] Cleared {removed} note(s) from '{}'", args.track);
    }
    emit_json(&json!({ "ok": true, "track_id": args.track, "removed": removed }));
    0
}

fn cmd_set_instrument(args: SetInstrumentArgs, common: &CommonOpts) -> u8 {
    let mut song = match core::load(&args.path) {
        Ok(s) => s,
        Err(e) => return emit_error(&e),
    };
    let params = match args.params_json.as_deref() {
        None => Map::new(),
        Some(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Object(m)) => m,
            Ok(_) => {
                emit_json(&json!({
                    "ok": false,
                    "errors": [{ "code": "INVALID_JSON", "message": "--params-json must be a JSON object" }]
                }));
                return 1;
            }
            Err(e) => return emit_error(&CodettaError::InvalidJson(e)),
        },
    };
    let new_instrument = Instrument {
        kind: args.kind.clone(),
        params,
    };
    let prev = match core::set_instrument(&mut song, &args.track, new_instrument) {
        Ok(p) => p,
        Err(e) => return emit_error(&e),
    };
    if let Some(code) = save_after_validate(&song, &args.path, common) {
        return code;
    }
    if !common.quiet {
        eprintln!(
            "[OK] Set instrument on '{}': {} → {}",
            args.track, prev, args.kind
        );
    }
    emit_json(&json!({
        "ok": true,
        "track_id": args.track,
        "instrument": args.kind,
        "previous": prev,
    }));
    0
}

fn cmd_set_fx(args: SetFxArgs, common: &CommonOpts) -> u8 {
    let mut song = match core::load(&args.path) {
        Ok(s) => s,
        Err(e) => return emit_error(&e),
    };
    let fx = match load_fx(args.fx_json.as_deref(), args.fx_file.as_deref()) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let count = match core::set_fx(&mut song, &args.track, fx) {
        Ok(n) => n,
        Err(e) => return emit_error(&e),
    };
    if let Some(code) = save_after_validate(&song, &args.path, common) {
        return code;
    }
    if !common.quiet {
        eprintln!("[OK] Set {count} fx on track '{}'", args.track);
    }
    emit_json(&json!({ "ok": true, "track_id": args.track, "fx_count": count }));
    0
}

fn cmd_edit_notes(args: EditNotesArgs, common: &CommonOpts) -> u8 {
    let mut song = match core::load(&args.path) {
        Ok(s) => s,
        Err(e) => return emit_error(&e),
    };
    let ops = match load_ops(args.ops_json.as_deref(), args.ops_file.as_deref()) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let stats = match core::edit_notes(&mut song, &args.track, &ops) {
        Ok(s) => s,
        Err(e) => return emit_error(&e),
    };
    if let Some(code) = save_after_validate(&song, &args.path, common) {
        return code;
    }
    if !common.quiet {
        eprintln!(
            "[OK] Applied {} op(s) on '{}' ({} note-touches)",
            stats.ops_applied, args.track, stats.notes_affected
        );
    }
    emit_json(&json!({
        "ok": true,
        "track_id": args.track,
        "ops_applied": stats.ops_applied,
        "notes_affected": stats.notes_affected,
    }));
    0
}

fn load_ops(json_str: Option<&str>, file: Option<&Path>) -> Result<Vec<NoteOp>, u8> {
    let raw: String = match (json_str, file) {
        (Some(s), _) => s.to_string(),
        (None, Some(p)) => match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(emit_error(&CodettaError::FileNotFound(p.to_path_buf())));
            }
            Err(e) => return Err(emit_error(&CodettaError::Io(e))),
        },
        (None, None) => {
            emit_json(&json!({
                "ok": false,
                "errors": [{ "code": "INVALID_JSON", "message": "either --ops-json or --ops-file is required" }]
            }));
            return Err(2);
        }
    };
    match serde_json::from_str::<Vec<NoteOp>>(&raw) {
        Ok(v) => Ok(v),
        Err(e) => Err(emit_error(&CodettaError::InvalidJson(e))),
    }
}

/// `--fx-json` / `--fx-file` から `Vec<Effect>` を取り出す。
/// 両方未指定なら空配列 (= 「fx チェーンをクリア」)。
fn load_fx(json_str: Option<&str>, file: Option<&Path>) -> Result<Vec<Effect>, u8> {
    let raw: String = match (json_str, file) {
        (Some(s), _) => s.to_string(),
        (None, Some(p)) => match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(emit_error(&CodettaError::FileNotFound(p.to_path_buf())));
            }
            Err(e) => return Err(emit_error(&CodettaError::Io(e))),
        },
        (None, None) => "[]".to_string(),
    };
    match serde_json::from_str::<Vec<Effect>>(&raw) {
        Ok(v) => Ok(v),
        Err(e) => Err(emit_error(&CodettaError::InvalidJson(e))),
    }
}

/// `--notes-json` / `--notes-file` のいずれかから `Vec<Note>` を取り出す。
/// 両方未指定なら空配列 (= 「ノートをクリア」 と同等)。
fn load_notes(json_str: Option<&str>, file: Option<&Path>) -> Result<Vec<Note>, u8> {
    let raw: String = match (json_str, file) {
        (Some(s), _) => s.to_string(),
        (None, Some(p)) => match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(emit_error(&CodettaError::FileNotFound(p.to_path_buf())));
            }
            Err(e) => return Err(emit_error(&CodettaError::Io(e))),
        },
        (None, None) => "[]".to_string(),
    };
    match serde_json::from_str::<Vec<Note>>(&raw) {
        Ok(n) => Ok(n),
        Err(e) => Err(emit_error(&CodettaError::InvalidJson(e))),
    }
}

/// 編集後の Song を validate し、 OK なら force 上書き保存する。
/// 失敗時の exit code を `Some(code)` で返し、 成功時は `None`。
fn save_after_validate(song: &Song, path: &Path, common: &CommonOpts) -> Option<u8> {
    let verrs = core::validate(song);
    if !verrs.is_empty() {
        if !common.quiet {
            eprintln!("[ERROR] {} validation error(s) after edit", verrs.len());
        }
        emit_json(&json!({ "ok": false, "errors": verrs }));
        return Some(1);
    }
    if let Err(e) = core::save(song, path, true) {
        return Some(emit_error(&e));
    }
    None
}

/// stdout に 1 行 JSON で書き出す (改行付き)。
fn emit_json(v: &Value) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, v).expect("write stdout");
    writeln!(out).expect("write stdout newline");
}

/// CodettaError を JSON エラーとして stdout に出し、 推奨 exit code を返す。
fn emit_error(e: &CodettaError) -> u8 {
    let (code, exit, msg) = match e {
        CodettaError::FileNotFound(p) => (
            "FILE_NOT_FOUND",
            3_u8,
            format!("file not found: {}", p.display()),
        ),
        CodettaError::FileExists(p) => (
            "FILE_EXISTS",
            1,
            format!("file already exists: {} (use --force)", p.display()),
        ),
        CodettaError::InvalidJson(je) => ("INVALID_JSON", 1, format!("invalid JSON: {je}")),
        CodettaError::Io(io) => ("IO_ERROR", 3, io.to_string()),
        CodettaError::UnknownVersion(v) => (
            "UNKNOWN_VERSION",
            1,
            format!("unsupported schema version: {v:?}"),
        ),
        CodettaError::TrackNotFound(id) => (
            "TRACK_NOT_FOUND",
            1,
            format!("track not found: {id:?}"),
        ),
        CodettaError::TrackIdDuplicate(id) => (
            "TRACK_ID_DUPLICATE",
            1,
            format!("duplicate track id: {id:?}"),
        ),
        CodettaError::Validation(errs) => {
            emit_json(&json!({ "ok": false, "errors": errs }));
            return 1;
        }
        CodettaError::Wav(we) => ("RENDER_FAILED", 1, format!("WAV write failed: {we}")),
        CodettaError::Render(m) => ("RENDER_FAILED", 1, m.clone()),
    };
    emit_json(&json!({
        "ok": false,
        "errors": [{ "code": code, "message": msg }]
    }));
    exit
}
