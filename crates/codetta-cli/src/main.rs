//! Codetta CLI entry point.
//!
//! 設計: docs/design/03-cli.md
//!
//! Phase 0 現スコープ: `new` / `info` / `validate` / `render` + track/notes 編集系。
//! 残コマンド (`set-instrument` / `set-fx` / `edit-notes` / `schema` 他) は続く実装で追加する。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use codetta_core::{
    self as core, CodettaError, Effect, Instrument, Note, NoteOp, Song, Track,
    KNOWN_DRUM_KEYS, KNOWN_EFFECT_TYPES, KNOWN_INSTRUMENT_TYPES,
};
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
    /// 利用可能な楽器一覧と各 type のパラメータスキーマを JSON 出力
    ListInstruments,
    /// 利用可能なエフェクト一覧と各 type のパラメータスキーマを JSON 出力
    ListEffects,
    /// プロジェクトファイル (.codetta) の JSON Schema を出力
    Schema,
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
        Command::ListInstruments => cmd_list_instruments(),
        Command::ListEffects => cmd_list_effects(),
        Command::Schema => cmd_schema(),
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

fn cmd_list_instruments() -> u8 {
    emit_json(&json!({
        "ok": true,
        "instruments": instrument_catalog(),
    }));
    0
}

fn cmd_list_effects() -> u8 {
    emit_json(&json!({
        "ok": true,
        "effects": effect_catalog(),
    }));
    0
}

fn cmd_schema() -> u8 {
    emit_json(&json!({
        "ok": true,
        "schema_version": core::SCHEMA_VERSION,
        "schema": song_json_schema(),
    }));
    0
}

/// 全 melodic / drum 楽器の説明 + 各 type のパラメータスキーマを構築する。
///
/// `KNOWN_INSTRUMENT_TYPES` に並ぶ全 type を一度走査して 1 エントリずつ返すので、
/// 既知 type を追加したら必ずここにも description / params を足す
/// (LLM / MCP がパラメータを引けないと話にならない)。
fn instrument_catalog() -> Vec<Value> {
    let adsr_params = json!({
        "attack":  { "type": "float", "default": 0.01, "range": [0.0, 10.0], "unit": "sec" },
        "decay":   { "type": "float", "default": 0.1,  "range": [0.0, 10.0], "unit": "sec" },
        "sustain": { "type": "float", "default": 0.7,  "range": [0.0, 1.0] },
        "release": { "type": "float", "default": 0.2,  "range": [0.0, 10.0], "unit": "sec" },
    });

    KNOWN_INSTRUMENT_TYPES
        .iter()
        .map(|&t| match t {
            "sin" => json!({
                "type": "sin",
                "category": "melodic",
                "description": "純音 (倍音なし)。 サブベース / パッド / FM 素材",
                "params": adsr_params,
            }),
            "saw" | "saw_lead" => json!({
                "type": t,
                "category": "melodic",
                "description": "PolyBLEP saw。 リード / ベース / パッド (倍音豊富)",
                "params": adsr_params,
            }),
            "square" | "square_bass" => json!({
                "type": t,
                "category": "melodic",
                "description": "PolyBLEP pulse。 デューティ比可変、 8bit / チップチューン",
                "params": {
                    "attack":  adsr_params["attack"],
                    "decay":   adsr_params["decay"],
                    "sustain": adsr_params["sustain"],
                    "release": adsr_params["release"],
                    "pulse_width": { "type": "float", "default": 0.5, "range": [0.05, 0.95] },
                },
            }),
            "triangle" => json!({
                "type": "triangle",
                "category": "melodic",
                "description": "三角波 (解析式)。 柔らかい音、 笛系",
                "params": adsr_params,
            }),
            "saw_pad" => json!({
                "type": "saw_pad",
                "category": "melodic",
                "description": "saw × 3 detune。 厚みあるパッド",
                "params": {
                    "attack":  adsr_params["attack"],
                    "decay":   adsr_params["decay"],
                    "sustain": adsr_params["sustain"],
                    "release": adsr_params["release"],
                    "detune_cents": { "type": "float", "default": 10.0, "range": [0.0, 50.0], "unit": "cent" },
                },
            }),
            "drum_kit" => json!({
                "type": "drum_kit",
                "category": "drum",
                "description": "GM Drum 互換キーの全合成ドラム。 pitch には drum key (例: \"kick\") を渡す",
                "params": {
                    "kit": {
                        "type": "string",
                        "default": "default",
                        "enum": ["default", "808", "909"],
                        "note": "kit バリエーションは kick / snare のみ反映 (他の voice は default 固定)",
                    },
                },
                "drum_keys": KNOWN_DRUM_KEYS,
            }),
            _ => json!({
                "type": t,
                "category": "unknown",
                "description": "(catalog entry missing — please update instrument_catalog)",
                "params": {},
            }),
        })
        .collect()
}

/// 全エフェクトの説明 + パラメータスキーマ。 `KNOWN_EFFECT_TYPES` に追加したらここも更新する。
fn effect_catalog() -> Vec<Value> {
    KNOWN_EFFECT_TYPES
        .iter()
        .map(|&t| match t {
            "lowpass" => json!({
                "type": "lowpass",
                "description": "Chamberlin SVF lowpass",
                "params": {
                    "cutoff": { "type": "float", "default": 1000.0, "range": [20.0, 20000.0], "unit": "Hz" },
                    "q":      { "type": "float", "default": 1.0,    "range": [0.5, 10.0] },
                },
            }),
            "highpass" => json!({
                "type": "highpass",
                "description": "Chamberlin SVF highpass",
                "params": {
                    "cutoff": { "type": "float", "default": 1000.0, "range": [20.0, 20000.0], "unit": "Hz" },
                    "q":      { "type": "float", "default": 1.0,    "range": [0.5, 10.0] },
                },
            }),
            "distortion" => json!({
                "type": "distortion",
                "description": "tanh ソフトクリップ + tone (内蔵 lowpass)",
                "params": {
                    "amount": { "type": "float", "default": 0.3, "range": [0.0, 1.0] },
                    "tone":   { "type": "float", "default": 0.5, "range": [0.0, 1.0] },
                },
            }),
            "delay" => json!({
                "type": "delay",
                "description": "循環バッファ delay (BPM 同期 / 秒指定)",
                "params": {
                    "time":     { "type": ["string", "float"], "default": "1/8",
                                  "note": "BPM 同期は \"1/4\" / \"1/8\" 等、 秒指定は数値 (0.001-2.0)" },
                    "feedback": { "type": "float", "default": 0.3,  "range": [0.0, 0.95] },
                    "mix":      { "type": "float", "default": 0.25, "range": [0.0, 1.0] },
                },
            }),
            "reverb" => json!({
                "type": "reverb",
                "description": "Schroeder reverb (4 comb + 2 allpass、 L/R で delay 長を分けた擬似ステレオ)",
                "params": {
                    "size": { "type": "float", "default": 0.5, "range": [0.0, 1.0] },
                    "damp": { "type": "float", "default": 0.5, "range": [0.0, 1.0] },
                    "mix":  { "type": "float", "default": 0.2, "range": [0.0, 1.0] },
                },
            }),
            _ => json!({
                "type": t,
                "description": "(catalog entry missing — please update effect_catalog)",
                "params": {},
            }),
        })
        .collect()
}

/// プロジェクトファイル (Song) の JSON Schema (draft-2020-12)。
///
/// LLM / MCP / IDE 補完で利用するための機械可読仕様。 schema の精度は
/// 「ある程度の指針」 程度で十分 (詳細な params 検証は `validate` 側で実施)。
fn song_json_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://codetta.dev/schemas/song-0.1.json",
        "title": "Codetta Song",
        "type": "object",
        "required": ["version", "metadata"],
        "properties": {
            "version":  { "type": "string", "enum": core::SUPPORTED_VERSIONS },
            "metadata": { "$ref": "#/$defs/Metadata" },
            "tracks":   { "type": "array", "items": { "$ref": "#/$defs/Track" } },
        },
        "$defs": {
            "Metadata": {
                "type": "object",
                "required": ["name", "bpm"],
                "properties": {
                    "name":           { "type": "string" },
                    "bpm":            { "type": "integer", "minimum": 20, "maximum": 300 },
                    "key":            { "type": "string", "description": "例: \"Am\", \"C\", \"F#m\"" },
                    "time_signature": {
                        "type": "array",
                        "items": { "type": "integer", "minimum": 1 },
                        "minItems": 2,
                        "maxItems": 2,
                    },
                    "created_at": { "type": "string" },
                    "tags":       { "type": "array", "items": { "type": "string" } },
                },
            },
            "Track": {
                "type": "object",
                "required": ["id", "name", "instrument"],
                "properties": {
                    "id":         { "type": "string", "minLength": 1 },
                    "name":       { "type": "string" },
                    "instrument": { "$ref": "#/$defs/Instrument" },
                    "volume":     { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                    "pan":        { "type": "number", "minimum": -1.0, "maximum": 1.0 },
                    "mute":       { "type": "boolean" },
                    "solo":       { "type": "boolean" },
                    "fx":         { "type": "array", "items": { "$ref": "#/$defs/Effect" } },
                    "notes":      { "type": "array", "items": { "$ref": "#/$defs/Note" } },
                },
            },
            "Instrument": {
                "type": "object",
                "required": ["type"],
                "properties": {
                    "type":   { "type": "string", "enum": KNOWN_INSTRUMENT_TYPES },
                    "params": { "type": "object", "additionalProperties": true },
                },
            },
            "Effect": {
                "type": "object",
                "required": ["type"],
                "properties": {
                    "type": { "type": "string", "enum": KNOWN_EFFECT_TYPES },
                },
                "additionalProperties": true,
            },
            "Note": {
                "type": "object",
                "required": ["t", "pitch", "dur"],
                "properties": {
                    "t":     { "type": "number", "minimum": 0.0 },
                    "pitch": { "$ref": "#/$defs/Pitch" },
                    "dur":   { "type": "number", "exclusiveMinimum": 0.0 },
                    "vel":   { "type": "integer", "minimum": 0, "maximum": 127 },
                },
            },
            "Pitch": {
                "oneOf": [
                    { "type": "integer", "minimum": 0, "maximum": 127, "description": "MIDI ノート番号" },
                    { "type": "string", "description": "ノート名 (例: \"C4\", \"Bb3\") または drum key (例: \"kick\")" },
                ],
            },
        },
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instrument_catalog_covers_all_known_types() {
        let catalog = instrument_catalog();
        let listed: std::collections::HashSet<&str> = catalog
            .iter()
            .map(|e| e["type"].as_str().expect("type field"))
            .collect();
        for t in KNOWN_INSTRUMENT_TYPES {
            assert!(listed.contains(t), "missing catalog entry for {t}");
            // 「catalog entry missing」 プレースホルダが残っていない
            let entry = catalog
                .iter()
                .find(|e| e["type"].as_str() == Some(t))
                .unwrap();
            let desc = entry["description"].as_str().unwrap_or("");
            assert!(
                !desc.contains("catalog entry missing"),
                "placeholder description for {t}: {desc}"
            );
            assert_eq!(catalog.iter().filter(|e| e["type"].as_str() == Some(t)).count(), 1, "duplicate entry for {t}");
        }
        assert_eq!(catalog.len(), KNOWN_INSTRUMENT_TYPES.len());
    }

    #[test]
    fn effect_catalog_covers_all_known_types() {
        let catalog = effect_catalog();
        let listed: std::collections::HashSet<&str> = catalog
            .iter()
            .map(|e| e["type"].as_str().expect("type field"))
            .collect();
        for t in KNOWN_EFFECT_TYPES {
            assert!(listed.contains(t), "missing catalog entry for {t}");
            let entry = catalog
                .iter()
                .find(|e| e["type"].as_str() == Some(t))
                .unwrap();
            let desc = entry["description"].as_str().unwrap_or("");
            assert!(
                !desc.contains("catalog entry missing"),
                "placeholder description for {t}: {desc}"
            );
        }
        assert_eq!(catalog.len(), KNOWN_EFFECT_TYPES.len());
    }

    #[test]
    fn drum_kit_entry_lists_all_drum_keys() {
        let catalog = instrument_catalog();
        let drum = catalog
            .iter()
            .find(|e| e["type"].as_str() == Some("drum_kit"))
            .expect("drum_kit catalog entry");
        let keys: Vec<&str> = drum["drum_keys"]
            .as_array()
            .expect("drum_keys array")
            .iter()
            .map(|v| v.as_str().expect("drum key string"))
            .collect();
        for k in KNOWN_DRUM_KEYS {
            assert!(keys.contains(k), "drum_kit catalog missing key {k}");
        }
    }

    #[test]
    fn schema_has_expected_top_level_defs() {
        let schema = song_json_schema();
        let defs = schema["$defs"]
            .as_object()
            .expect("schema $defs");
        for required in [
            "Metadata", "Track", "Instrument", "Effect", "Note", "Pitch",
        ] {
            assert!(defs.contains_key(required), "schema missing $defs.{required}");
        }
        // top level
        assert_eq!(schema["type"].as_str(), Some("object"));
        let required: Vec<&str> = schema["required"]
            .as_array()
            .expect("required")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"version"));
        assert!(required.contains(&"metadata"));
    }

    #[test]
    fn schema_instrument_enum_matches_known_types() {
        let schema = song_json_schema();
        let inst_enum = schema["$defs"]["Instrument"]["properties"]["type"]["enum"]
            .as_array()
            .expect("instrument type enum");
        let listed: Vec<&str> = inst_enum.iter().filter_map(|v| v.as_str()).collect();
        for t in KNOWN_INSTRUMENT_TYPES {
            assert!(listed.contains(t), "schema instrument enum missing {t}");
        }
    }
}
