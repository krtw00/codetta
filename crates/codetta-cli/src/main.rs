//! Codetta CLI entry point.
//!
//! 設計: docs/design/03-cli.md
//!
//! Phase 0 現スコープ: `new` / `info` / `validate` / `render` + track/notes 編集系。
//! 残コマンド (`set-instrument` / `set-fx` / `edit-notes` / `schema` 他) は続く実装で追加する。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{ArgGroup, Args, Parser, Subcommand};
use codetta_core::{
    self as core,
    midi::{ExtensionsMode, MidiError, MidiExportOptions, MidiImportOptions, DEFAULT_PPQ},
    migrate::{MigrateError, DEFAULT_SF2},
    synth::soundfont::{list_soundfont_presets, resolve_soundfont_path, SoundFontError},
    CodettaError, Effect, Instrument, Note, NoteOp, Song, Track, ValidationError, KNOWN_DRUM_KEYS,
    KNOWN_EFFECT_TYPES, KNOWN_INSTRUMENT_TYPES,
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
    /// プロジェクトの master_gain を変更
    SetMasterGain(SetMasterGainArgs),
    /// ノートに対する一括変形 (transpose / shift / scale / quantize 等)
    EditNotes(EditNotesArgs),
    /// 利用可能な楽器一覧と各 type のパラメータスキーマを JSON 出力
    ListInstruments,
    /// 利用可能なエフェクト一覧と各 type のパラメータスキーマを JSON 出力
    ListEffects,
    /// SoundFont (.sf2) の preset 一覧 + メタ情報を JSON 出力
    ListSoundfontPresets(ListSoundfontPresetsArgs),
    /// プロジェクトファイル (.codetta) の JSON Schema を出力
    Schema,
    /// 旧 schema (0.1) を最新 (0.2) に変換し、 内蔵 synth を SF2 preset にマップする
    Migrate(MigrateArgs),
    /// 標準 MIDI ファイル (.mid) を `.codetta` (0.2) に取り込む
    ImportMidi(ImportMidiArgs),
    /// `.codetta` を標準 MIDI ファイル (.mid、 SMF Type 1) に書き出す
    ExportMidi(ExportMidiArgs),
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
    /// 全 track 合算後 (soft_clip 前) に乗算する master gain。 0.0..=4.0、 デフォルト 1.0。
    /// voice 密度別の目安: 単 track or 薄いアレンジ=2.0 / 2 track 中音域=1.5-1.8 /
    /// 3 track 以上で chord pad + percussion 同時発音=1.0-1.2 (peak overflow + soft_clip の歪み回避)
    #[arg(long = "master-gain")]
    master_gain: Option<f32>,
    /// 既存ファイルを上書き
    #[arg(long)]
    force: bool,
}

fn parse_time_sig(s: &str) -> Result<[u32; 2], String> {
    let (n, d) = s
        .split_once('/')
        .ok_or_else(|| "expected N/D (e.g. 4/4)".to_string())?;
    let n: u32 = n
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    let d: u32 = d
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
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
    /// 楽器 type。 schema 0.2 では `soundfont` 1 種のみ (= デフォルト)。
    /// SF2 path や preset は `--params-json` で指定する (例:
    /// `--params-json '{"file":"GeneralUser-GS-v1.471.sf2","preset":0,"bank":0}'`)。
    #[arg(long, default_value = "soundfont")]
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
struct SetMasterGainArgs {
    path: PathBuf,
    /// 全 track 合算後 (soft_clip 前) に乗算する master gain。 0.0..=4.0
    #[arg(long)]
    value: f32,
}

#[derive(Args)]
struct ListSoundfontPresetsArgs {
    /// SF2 ファイル path (絶対 or `$CODETTA_SOUNDFONT_DIR` 配下の相対)
    file: PathBuf,
}

#[derive(Args)]
#[command(group(
    ArgGroup::new("migrate_dest")
        .args(["output", "in_place"])
        .required(true)
        .multiple(false),
))]
struct MigrateArgs {
    /// 入力 `.codetta` ファイル (schema 0.1)
    path: PathBuf,
    /// 出力先 `.codetta` ファイル (`--in-place` と排他)
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// 入力ファイルを上書きする (`--output` と排他)
    #[arg(long = "in-place")]
    in_place: bool,
    /// LUT 適用後の `soundfont` params.file に書き込む SF2 ファイル名 (省略時は default SF2)
    #[arg(long, default_value = DEFAULT_SF2)]
    sf2: String,
}

#[derive(Args)]
struct ExportMidiArgs {
    /// 入力 `.codetta` ファイル (schema 0.2、 0.1 は in-memory migrate で 0.2 化してから出力)
    path: PathBuf,
    /// 出力 `.mid` ファイル
    #[arg(short, long)]
    output: PathBuf,
    /// 拡張属性 (master_gain / fx / SF2 preset 詳細) の書き出しモード
    #[arg(long, value_parser = parse_extensions_mode, default_value = "text-meta")]
    extensions: ExtensionsMode,
    /// PPQ (ticks per quarter)。 default 480 (ADR L43)
    #[arg(long, default_value_t = DEFAULT_PPQ)]
    ppq: u16,
    /// 0.1 入力の in-memory migrate 時に soundfont params.file に書き込む SF2 ファイル名
    #[arg(long, default_value = DEFAULT_SF2)]
    sf2: String,
    /// 既存 `.mid` を上書き
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
struct ImportMidiArgs {
    /// 入力 `.mid` ファイル
    path: PathBuf,
    /// 出力 `.codetta` ファイル
    #[arg(short, long)]
    output: PathBuf,
    /// 拡張属性 (master_gain / fx / SF2 preset 詳細) の取り出しモード
    #[arg(long, value_parser = parse_extensions_mode, default_value = "text-meta")]
    extensions: ExtensionsMode,
    /// `Instrument.params.file` に書く SF2 ファイル名 (省略時は default SF2)。
    /// 指定があれば import 時に preset 存在確認も行い、 見つからなければ preset 0 fallback + warning。
    #[arg(long, default_value = DEFAULT_SF2)]
    sf2: String,
    /// 生成される Song の `metadata.name` (省略時は MIDI path の stem)
    #[arg(long)]
    name: Option<String>,
    /// 既存 `.codetta` を上書き
    #[arg(long)]
    force: bool,
}

fn parse_extensions_mode(s: &str) -> Result<ExtensionsMode, String> {
    match s {
        "text-meta" => Ok(ExtensionsMode::TextMeta),
        "sidecar" => Ok(ExtensionsMode::Sidecar),
        "none" => Ok(ExtensionsMode::None),
        other => Err(format!(
            "unknown extensions mode {other:?} (expected text-meta / sidecar / none)"
        )),
    }
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
        Command::SetMasterGain(a) => cmd_set_master_gain(a, &cli.common),
        Command::EditNotes(a) => cmd_edit_notes(a, &cli.common),
        Command::ListInstruments => cmd_list_instruments(),
        Command::ListEffects => cmd_list_effects(),
        Command::ListSoundfontPresets(a) => cmd_list_soundfont_presets(a, &cli.common),
        Command::Schema => cmd_schema(),
        Command::Migrate(a) => cmd_migrate(a, &cli.common),
        Command::ImportMidi(a) => cmd_import_midi(a, &cli.common),
        Command::ExportMidi(a) => cmd_export_midi(a, &cli.common),
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
    if let Some(mg) = args.master_gain {
        song.metadata.master_gain = mg;
    }

    if !common.quiet {
        eprintln!("[INFO] Creating {}", args.path.display());
    }

    if args.path.exists() && !args.force {
        return emit_error(&CodettaError::FileExists(args.path.clone()));
    }
    let warnings = match save_after_validate(&song, &args.path, common) {
        Ok(w) => w,
        Err(code) => return code,
    };

    let abs = std::fs::canonicalize(&args.path).unwrap_or_else(|_| args.path.clone());
    emit_json(&with_warnings(
        json!({
            "ok": true,
            "path": abs.to_string_lossy(),
            "version": core::SCHEMA_VERSION,
        }),
        &warnings,
    ));
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
    let (errors, warnings) = partition_validation(core::validate(&song));
    if errors.is_empty() {
        if !common.quiet {
            if warnings.is_empty() {
                eprintln!("[OK] {} is valid", args.path.display());
            } else {
                eprintln!(
                    "[OK] {} is valid ({} warning(s))",
                    args.path.display(),
                    warnings.len()
                );
            }
        }
        emit_json(&validation_payload(true, &errors, &warnings));
        0
    } else {
        if !common.quiet {
            eprintln!(
                "[ERROR] {} error(s), {} warning(s)",
                errors.len(),
                warnings.len()
            );
        }
        emit_json(&validation_payload(false, &errors, &warnings));
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
    let (errors, warnings) = partition_validation(core::validate(&song));
    if !errors.is_empty() {
        if !common.quiet {
            eprintln!(
                "[ERROR] {} error(s), {} warning(s)",
                errors.len(),
                warnings.len()
            );
        }
        emit_json(&validation_payload(false, &errors, &warnings));
        return 1;
    }
    if !warnings.is_empty() && !common.quiet {
        eprintln!("[WARN] {} warning(s) — rendering anyway", warnings.len());
    }

    if !common.quiet {
        eprintln!(
            "[INFO] Rendering {} → {}",
            args.path.display(),
            args.output.display()
        );
    }

    let t0 = std::time::Instant::now();
    let stats = match core::render_to_wav(&song, &args.output) {
        Ok(s) => s,
        Err(e) => return emit_error(&e),
    };
    let elapsed = t0.elapsed().as_secs_f32();
    let rtfactor = if elapsed > 0.0 {
        stats.duration_sec / elapsed
    } else {
        0.0
    };

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
    let mut payload = json!({
        "ok": true,
        "output": abs.to_string_lossy(),
        "duration_sec": stats.duration_sec,
        "sample_rate": stats.sample_rate,
        "bit_depth": stats.bit_depth,
        "render_time_sec": elapsed,
        "rtfactor": rtfactor,
    });
    if !warnings.is_empty() {
        payload["warnings"] = json!(warnings);
    }
    emit_json(&payload);
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
    let warnings = match save_after_validate(&song, &args.path, common) {
        Ok(w) => w,
        Err(code) => return code,
    };

    if !common.quiet {
        eprintln!("[OK] Added track '{}'", args.id);
    }
    emit_json(&with_warnings(
        json!({ "ok": true, "track_id": args.id }),
        &warnings,
    ));
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
    let warnings = match save_after_validate(&song, &args.path, common) {
        Ok(w) => w,
        Err(code) => return code,
    };
    if !common.quiet {
        eprintln!("[OK] Removed track '{}'", args.id);
    }
    emit_json(&with_warnings(
        json!({ "ok": true, "track_id": args.id }),
        &warnings,
    ));
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
    let warnings = match save_after_validate(&song, &args.path, common) {
        Ok(w) => w,
        Err(code) => return code,
    };
    if !common.quiet {
        eprintln!("[OK] Set {count} note(s) on track '{}'", args.track);
    }
    emit_json(&with_warnings(
        json!({ "ok": true, "track_id": args.track, "note_count": count }),
        &warnings,
    ));
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
    let warnings = match save_after_validate(&song, &args.path, common) {
        Ok(w) => w,
        Err(code) => return code,
    };
    if !common.quiet {
        eprintln!(
            "[OK] Added {added} note(s) to '{}' (skipped {skipped} dup, total {total})",
            args.track
        );
    }
    emit_json(&with_warnings(
        json!({
            "ok": true,
            "track_id": args.track,
            "added": added,
            "skipped_duplicates": skipped,
            "total_notes": total,
        }),
        &warnings,
    ));
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
    let warnings = match save_after_validate(&song, &args.path, common) {
        Ok(w) => w,
        Err(code) => return code,
    };
    if !common.quiet {
        eprintln!("[OK] Cleared {removed} note(s) from '{}'", args.track);
    }
    emit_json(&with_warnings(
        json!({ "ok": true, "track_id": args.track, "removed": removed }),
        &warnings,
    ));
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
    let warnings = match save_after_validate(&song, &args.path, common) {
        Ok(w) => w,
        Err(code) => return code,
    };
    if !common.quiet {
        eprintln!(
            "[OK] Set instrument on '{}': {} → {}",
            args.track, prev, args.kind
        );
    }
    emit_json(&with_warnings(
        json!({
            "ok": true,
            "track_id": args.track,
            "instrument": args.kind,
            "previous": prev,
        }),
        &warnings,
    ));
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
    let warnings = match save_after_validate(&song, &args.path, common) {
        Ok(w) => w,
        Err(code) => return code,
    };
    if !common.quiet {
        eprintln!("[OK] Set {count} fx on track '{}'", args.track);
    }
    emit_json(&with_warnings(
        json!({ "ok": true, "track_id": args.track, "fx_count": count }),
        &warnings,
    ));
    0
}

fn cmd_set_master_gain(args: SetMasterGainArgs, common: &CommonOpts) -> u8 {
    let mut song = match core::load(&args.path) {
        Ok(s) => s,
        Err(e) => return emit_error(&e),
    };
    let prev = song.metadata.master_gain;
    song.metadata.master_gain = args.value;
    let warnings = match save_after_validate(&song, &args.path, common) {
        Ok(w) => w,
        Err(code) => return code,
    };
    if !common.quiet {
        eprintln!("[OK] master_gain {prev} -> {}", args.value);
    }
    emit_json(&with_warnings(
        json!({ "ok": true, "master_gain": args.value, "previous": prev }),
        &warnings,
    ));
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
    let warnings = match save_after_validate(&song, &args.path, common) {
        Ok(w) => w,
        Err(code) => return code,
    };
    if !common.quiet {
        eprintln!(
            "[OK] Applied {} op(s) on '{}' ({} note-touches)",
            stats.ops_applied, args.track, stats.notes_affected
        );
    }
    emit_json(&with_warnings(
        json!({
            "ok": true,
            "track_id": args.track,
            "ops_applied": stats.ops_applied,
            "notes_affected": stats.notes_affected,
        }),
        &warnings,
    ));
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

fn cmd_list_soundfont_presets(args: ListSoundfontPresetsArgs, common: &CommonOpts) -> u8 {
    let resolved = resolve_soundfont_path(&args.file);
    if !common.quiet {
        eprintln!("[INFO] Reading SF2 {}", resolved.display());
    }
    match list_soundfont_presets(&resolved) {
        Ok((meta, presets)) => {
            let abs = std::fs::canonicalize(&resolved).unwrap_or_else(|_| resolved.clone());
            let presets_json: Vec<Value> = presets
                .iter()
                .map(|p| json!({ "bank": p.bank, "preset": p.preset, "name": p.name }))
                .collect();
            if !common.quiet {
                eprintln!("[OK] {} preset(s) found", presets.len());
            }
            emit_json(&json!({
                "ok": true,
                "file": abs.to_string_lossy(),
                "soundfont": {
                    "bank_name": meta.bank_name,
                    "version": meta.version,
                    "author": meta.author,
                    "copyright": meta.copyright,
                    "comments": meta.comments,
                },
                "preset_count": presets.len(),
                "presets": presets_json,
            }));
            0
        }
        Err(e) => emit_soundfont_error(&e),
    }
}

fn emit_soundfont_error(e: &SoundFontError) -> u8 {
    let (code, exit, msg) = match e {
        SoundFontError::NotFound(p) => (
            "FILE_NOT_FOUND",
            3_u8,
            format!("SF2 file not found: {}", p.display()),
        ),
        SoundFontError::Io { path, source } => (
            "IO_ERROR",
            3,
            format!("I/O error reading {}: {}", path.display(), source),
        ),
        SoundFontError::Parse { path, message } => (
            "SOUNDFONT_PARSE_FAILED",
            1,
            format!("Failed to parse SF2 {}: {}", path.display(), message),
        ),
        SoundFontError::Synth(m) => (
            "SOUNDFONT_PARSE_FAILED",
            1,
            format!("Synthesizer init error: {m}"),
        ),
    };
    emit_json(&json!({
        "ok": false,
        "errors": [{ "code": code, "message": msg }]
    }));
    exit
}

fn cmd_schema() -> u8 {
    emit_json(&json!({
        "ok": true,
        "schema_version": core::SCHEMA_VERSION,
        "schema": song_json_schema(),
    }));
    0
}

fn cmd_migrate(args: MigrateArgs, common: &CommonOpts) -> u8 {
    // migrate は SUPPORTED_VERSIONS の変動に独立させるため raw JSON で読む
    // (= io::load は version を SUPPORTED_VERSIONS で弾く)。
    let bytes = match std::fs::read(&args.path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return emit_error(&CodettaError::FileNotFound(args.path.clone()));
        }
        Err(e) => return emit_error(&CodettaError::Io(e)),
    };
    let input: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return emit_error(&CodettaError::InvalidJson(e)),
    };

    let outcome = match core::migrate_song_json(&input, Some(&args.sf2)) {
        Ok(o) => o,
        Err(e) => return emit_migrate_error(&e),
    };

    let output_path = if args.in_place {
        args.path.clone()
    } else {
        args.output
            .clone()
            .expect("ArgGroup 'migrate_dest' ensures --in-place or --output is set")
    };

    let mut serialized = match serde_json::to_vec_pretty(&outcome.song) {
        Ok(b) => b,
        Err(e) => return emit_error(&CodettaError::InvalidJson(e)),
    };
    serialized.push(b'\n');
    if let Err(e) = std::fs::write(&output_path, &serialized) {
        return emit_error(&CodettaError::Io(e));
    }

    if !common.quiet {
        let warn_tail = if outcome.warnings.is_empty() {
            String::new()
        } else {
            format!(", {} warning(s)", outcome.warnings.len())
        };
        eprintln!(
            "[OK] Migrated {} -> {} ({} -> {}, {} track(s) migrated{})",
            args.path.display(),
            output_path.display(),
            outcome.from_version,
            outcome.to_version,
            outcome.tracks_migrated,
            warn_tail,
        );
    }

    let abs_in = std::fs::canonicalize(&args.path).unwrap_or_else(|_| args.path.clone());
    let abs_out = std::fs::canonicalize(&output_path).unwrap_or_else(|_| output_path.clone());

    let mut payload = json!({
        "ok": true,
        "input": abs_in.to_string_lossy(),
        "output": abs_out.to_string_lossy(),
        "from_version": outcome.from_version,
        "to_version": outcome.to_version,
        "tracks_migrated": outcome.tracks_migrated,
        "instrument_mapping": outcome.instrument_mapping,
    });
    if !outcome.warnings.is_empty() {
        payload["warnings"] = json!(outcome.warnings);
    }
    emit_json(&payload);
    0
}

fn cmd_import_midi(args: ImportMidiArgs, common: &CommonOpts) -> u8 {
    if args.output.exists() && !args.force {
        return emit_error(&CodettaError::FileExists(args.output.clone()));
    }

    let options = MidiImportOptions {
        extensions: args.extensions,
        sf2_file: Some(args.sf2.clone()),
        song_name: args.name.clone(),
    };

    if !common.quiet {
        eprintln!(
            "[INFO] Importing {} -> {}",
            args.path.display(),
            args.output.display()
        );
    }

    let outcome = match core::import_midi(&args.path, &options) {
        Ok(o) => o,
        Err(e) => return emit_midi_error(&e),
    };

    let (errors, validate_warnings) = partition_validation(core::validate(&outcome.song));
    if !errors.is_empty() {
        if !common.quiet {
            eprintln!(
                "[ERROR] imported song failed validation: {} error(s), {} warning(s)",
                errors.len(),
                validate_warnings.len()
            );
        }
        emit_json(&validation_payload(false, &errors, &validate_warnings));
        return 1;
    }

    if let Err(e) = core::save(&outcome.song, &args.output, true) {
        return emit_error(&e);
    }

    let abs_in = std::fs::canonicalize(&args.path).unwrap_or_else(|_| args.path.clone());
    let abs_out = std::fs::canonicalize(&args.output).unwrap_or_else(|_| args.output.clone());

    if !common.quiet {
        eprintln!(
            "[OK] Imported {} -> {} ({}, ppq={}, {} track(s), {} import warning(s), {} validate warning(s))",
            abs_in.display(),
            abs_out.display(),
            outcome.source_format,
            outcome.ppq,
            outcome.song.tracks.len(),
            outcome.warnings.len(),
            validate_warnings.len(),
        );
    }

    let mut payload = json!({
        "ok": true,
        "input": abs_in.to_string_lossy(),
        "output": abs_out.to_string_lossy(),
        "source_format": outcome.source_format,
        "ppq": outcome.ppq,
        "track_count": outcome.song.tracks.len(),
        "extensions_recovered": outcome.extensions_recovered,
    });
    if !outcome.warnings.is_empty() {
        payload["warnings"] = json!(outcome.warnings);
    }
    if !validate_warnings.is_empty() {
        payload["validate_warnings"] = json!(validate_warnings);
    }
    emit_json(&payload);
    0
}

fn cmd_export_midi(args: ExportMidiArgs, common: &CommonOpts) -> u8 {
    if args.output.exists() && !args.force {
        return emit_error(&CodettaError::FileExists(args.output.clone()));
    }

    // 0.1 入力は in-memory migrate を挟む (ADR L273)。 io::load は SUPPORTED_VERSIONS=["0.2"]
    // で 0.1 を弾くので、 raw JSON で読んで version を見る。
    let bytes = match std::fs::read(&args.path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return emit_error(&CodettaError::FileNotFound(args.path.clone()));
        }
        Err(e) => return emit_error(&CodettaError::Io(e)),
    };
    let raw: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return emit_error(&CodettaError::InvalidJson(e)),
    };

    let raw_version = raw
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let (song_json, migrate_summary): (Value, Option<Value>) = if raw_version == "0.1" {
        match core::migrate_song_json(&raw, Some(&args.sf2)) {
            Ok(outcome) => {
                let summary = json!({
                    "from_version": outcome.from_version,
                    "to_version": outcome.to_version,
                    "tracks_migrated": outcome.tracks_migrated,
                    "instrument_mapping": outcome.instrument_mapping,
                    "warnings": outcome.warnings,
                });
                if !common.quiet {
                    eprintln!(
                        "[INFO] Implicit migrate 0.1 -> 0.2 (in-memory, {} track(s) migrated)",
                        outcome.tracks_migrated
                    );
                }
                (outcome.song, Some(summary))
            }
            Err(e) => return emit_migrate_error(&e),
        }
    } else {
        (raw, None)
    };

    let song: Song = match serde_json::from_value(song_json) {
        Ok(s) => s,
        Err(e) => return emit_error(&CodettaError::InvalidJson(e)),
    };

    // ADR L350: MIDI export 経路では `validate` を強制しない。
    // - SF2 file の物理存在 (= SOUNDFONT_FILE_NOT_FOUND) は MIDI 出力に無関係
    //   (= GM Program 番号さえあれば外部 DAW で別 SF2 に再 mapping できる)
    // - 構造健全性 (instrument が soundfont か / preset / pitch / bpm 等) は
    //   `export_song` 内部で必要な範囲だけ自前 check し、 不正は MidiError として返す。

    if !common.quiet {
        eprintln!(
            "[INFO] Exporting {} -> {} (ppq={}, extensions={})",
            args.path.display(),
            args.output.display(),
            args.ppq,
            args.extensions.as_str(),
        );
    }

    let options = MidiExportOptions {
        extensions: args.extensions,
        ppq: args.ppq,
    };
    let outcome = match core::export_song(&song, &args.output, &options) {
        Ok(o) => o,
        Err(e) => return emit_midi_error(&e),
    };

    let abs_in = std::fs::canonicalize(&args.path).unwrap_or_else(|_| args.path.clone());
    let abs_out = std::fs::canonicalize(&args.output).unwrap_or_else(|_| args.output.clone());

    if !common.quiet {
        let sidecar_tail = match &outcome.sidecar_written {
            Some(p) => format!(", sidecar={}", p.display()),
            None => String::new(),
        };
        eprintln!(
            "[OK] Exported {} -> {} ({}, {} track(s), text_meta={}{})",
            abs_in.display(),
            abs_out.display(),
            outcome.format,
            outcome.track_count,
            outcome.text_meta_written,
            sidecar_tail,
        );
    }

    let mut payload = json!({
        "ok": true,
        "input": abs_in.to_string_lossy(),
        "output": abs_out.to_string_lossy(),
        "format": outcome.format,
        "ppq": outcome.ppq,
        "track_count": outcome.track_count,
        "text_meta_written": outcome.text_meta_written,
    });
    if let Some(sidecar) = &outcome.sidecar_written {
        payload["sidecar"] = json!(sidecar.to_string_lossy());
    }
    if !outcome.warnings.is_empty() {
        payload["warnings"] = json!(outcome.warnings);
    }
    if let Some(summary) = migrate_summary {
        payload["implicit_migrate"] = summary;
    }
    emit_json(&payload);
    0
}

fn emit_midi_error(e: &MidiError) -> u8 {
    let (code, exit, msg, context) = match e {
        MidiError::FileNotFound(p) => (
            "FILE_NOT_FOUND",
            3_u8,
            format!("MIDI file not found: {}", p.display()),
            None,
        ),
        MidiError::Io(io) => ("IO_ERROR", 3, format!("MIDI I/O error: {io}"), None),
        MidiError::Parse(m) => (
            "MIDI_PARSE_FAILED",
            1,
            format!("MIDI parse failed: {m}"),
            None,
        ),
        MidiError::UnsupportedFormat(m) => (
            "MIDI_UNSUPPORTED_FORMAT",
            1,
            format!("unsupported MIDI format: {m}"),
            None,
        ),
        MidiError::UnsupportedTiming => (
            "MIDI_UNSUPPORTED_TIMING",
            1,
            "MIDI uses SMPTE timecode timing; only PPQ-based timing is supported".to_string(),
            None,
        ),
        MidiError::InvalidExtensions(m) => (
            "MIDI_INVALID_EXTENSIONS",
            1,
            format!("invalid codetta extensions JSON: {m}"),
            None,
        ),
        MidiError::TrackLimitExceeded(ids) => (
            "MIDI_TRACK_LIMIT_EXCEEDED",
            1,
            format!(
                "song has more melodic tracks than MIDI channels allow (15 melodic + 1 drum); \
                 excess tracks: {ids:?}. Reduce tracks or consolidate via set-instrument."
            ),
            Some(json!({ "excess_track_ids": ids })),
        ),
        MidiError::MultipleDrumTracksNotSupported(ids) => (
            "MIDI_MULTIPLE_DRUM_TRACKS",
            1,
            format!(
                "more than one drum track (bank=128) found; MIDI ch10 supports only one drum track. \
                 Drum track ids: {ids:?}"
            ),
            Some(json!({ "drum_track_ids": ids })),
        ),
        MidiError::InvalidNotePitch { track_id, reason } => (
            "MIDI_INVALID_NOTE_PITCH",
            1,
            format!("track {track_id:?}: {reason}"),
            Some(json!({ "track_id": track_id })),
        ),
        MidiError::UnsupportedInstrumentType { track_id, kind } => (
            "MIDI_UNSUPPORTED_INSTRUMENT",
            1,
            format!(
                "track {track_id:?} has instrument type {kind:?}; export expects 'soundfont' \
                 (schema 0.2). Run `codetta migrate` first if this is a 0.1 file."
            ),
            Some(json!({ "track_id": track_id, "instrument_type": kind })),
        ),
        MidiError::InvalidSoundFontParams { track_id, reason } => (
            "MIDI_INVALID_SOUNDFONT_PARAMS",
            1,
            format!("track {track_id:?}: {reason}"),
            Some(json!({ "track_id": track_id })),
        ),
    };
    let mut err_obj = json!({ "code": code, "message": msg });
    if let Some(ctx) = context {
        err_obj["context"] = ctx;
    }
    emit_json(&json!({
        "ok": false,
        "errors": [err_obj]
    }));
    exit
}

fn emit_migrate_error(e: &MigrateError) -> u8 {
    let (code, msg) = match e {
        MigrateError::MissingVersion => ("MIGRATE_MISSING_VERSION", e.to_string()),
        MigrateError::NotNeeded { .. } => ("MIGRATE_NOT_NEEDED", e.to_string()),
        MigrateError::UnsupportedVersion(_) => ("MIGRATE_UNSUPPORTED_VERSION", e.to_string()),
        MigrateError::MalformedTrack { .. } => ("MIGRATE_MALFORMED_TRACK", e.to_string()),
    };
    emit_json(&json!({
        "ok": false,
        "errors": [{ "code": code, "message": msg }]
    }));
    1
}

/// 全 melodic / drum 楽器の説明 + 各 type のパラメータスキーマを構築する。
///
/// `KNOWN_INSTRUMENT_TYPES` に並ぶ全 type を一度走査して 1 エントリずつ返すので、
/// 既知 type を追加したら必ずここにも description / params を足す
/// (LLM / MCP がパラメータを引けないと話にならない)。
fn instrument_catalog() -> Vec<Value> {
    KNOWN_INSTRUMENT_TYPES
        .iter()
        .map(|&t| match t {
            "soundfont" => json!({
                "type": "soundfont",
                "category": "sampler",
                "description": "外部 SoundFont (.sf2) ファイル経由のサンプラ。 schema 0.2 では唯一の楽器 type (07-soundfont.md)。 file は絶対 path か $CODETTA_SOUNDFONT_DIR (default ~/Music/sf2/) からの相対 path。 bank=128 は GM Drum kit (= drum_keys の要素名キーを pitch に書ける)",
                "params": {
                    "file": {
                        "type": "string",
                        "required": true,
                        "note": "SF2 ファイル path (絶対 or $CODETTA_SOUNDFONT_DIR 配下の相対)",
                    },
                    "preset": {
                        "type": "integer",
                        "default": 0,
                        "range": [0, 127],
                        "note": "GM Program 番号 (0 = Acoustic Grand Piano、 24 = Acoustic Guitar(nylon) など)",
                    },
                    "bank": {
                        "type": "integer",
                        "default": 0,
                        "range": [0, 128],
                        "note": "GM/GS bank (melodic は 0、 GS Drum は 128)",
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
                    "master_gain": {
                        "type": "number",
                        "minimum": 0.0,
                        "maximum": 4.0,
                        "description": "全 track ミックス後 (soft_clip 前) に乗算する gain。 default 1.0",
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
/// 失敗時は `Err(exit_code)`、 成功時は `Ok(warnings)` (Warning 級のみ、 caller の JSON 出力に乗せる)。
/// warnings は保存をブロックしない。
fn save_after_validate(
    song: &Song,
    path: &Path,
    common: &CommonOpts,
) -> Result<Vec<ValidationError>, u8> {
    let (errors, warnings) = partition_validation(core::validate(song));
    if !errors.is_empty() {
        if !common.quiet {
            eprintln!(
                "[ERROR] {} error(s), {} warning(s) after edit",
                errors.len(),
                warnings.len()
            );
        }
        emit_json(&validation_payload(false, &errors, &warnings));
        return Err(1);
    }
    if !warnings.is_empty() && !common.quiet {
        eprintln!("[WARN] {} warning(s) after edit", warnings.len());
    }
    if let Err(e) = core::save(song, path, true) {
        return Err(emit_error(&e));
    }
    Ok(warnings)
}

/// `Ok` payload に warnings 配列を追加 (空なら無加工)。
fn with_warnings(mut payload: Value, warnings: &[ValidationError]) -> Value {
    if !warnings.is_empty() {
        payload["warnings"] = json!(warnings);
    }
    payload
}

/// validate 結果を Error 級 と Warning 級 に分割。
fn partition_validation(
    issues: Vec<ValidationError>,
) -> (Vec<ValidationError>, Vec<ValidationError>) {
    issues.into_iter().partition(ValidationError::is_error)
}

/// 検証結果から CLI JSON payload を組み立てる。 warnings は非空のときだけ含める。
fn validation_payload(ok: bool, errors: &[ValidationError], warnings: &[ValidationError]) -> Value {
    let mut payload = json!({ "ok": ok });
    if !errors.is_empty() {
        payload["errors"] = json!(errors);
    }
    if !warnings.is_empty() {
        payload["warnings"] = json!(warnings);
    }
    payload
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
        CodettaError::TrackNotFound(id) => {
            ("TRACK_NOT_FOUND", 1, format!("track not found: {id:?}"))
        }
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
            assert_eq!(
                catalog
                    .iter()
                    .filter(|e| e["type"].as_str() == Some(t))
                    .count(),
                1,
                "duplicate entry for {t}"
            );
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
    fn catalog_params_match_instrument_param_keys() {
        // catalog の params キー集合 と validate 側 instrument_param_keys が一致することを保証。
        // どちらかを更新したらもう一方も更新する必要があり、 これが落ちたら同期漏れ。
        use codetta_core::validate::instrument_param_keys;
        use std::collections::HashSet;

        let catalog = instrument_catalog();
        for entry in &catalog {
            let kind = entry["type"].as_str().expect("type field");
            let params = entry["params"].as_object().expect("params field");
            let catalog_keys: HashSet<&str> = params.keys().map(String::as_str).collect();
            let known: HashSet<&str> = instrument_param_keys(kind)
                .unwrap_or_else(|| panic!("instrument_param_keys missing entry for {kind:?}"))
                .iter()
                .copied()
                .collect();
            assert_eq!(
                catalog_keys, known,
                "params mismatch for {kind:?}: catalog={catalog_keys:?} vs validate={known:?}"
            );
        }
    }

    #[test]
    fn catalog_params_match_effect_param_keys() {
        // catalog の params キー集合 と validate 側 effect_param_keys が一致することを保証。
        // どちらかを更新したらもう一方も更新する必要があり、 これが落ちたら同期漏れ。
        use codetta_core::validate::effect_param_keys;
        use std::collections::HashSet;

        let catalog = effect_catalog();
        for entry in &catalog {
            let kind = entry["type"].as_str().expect("type field");
            let params = entry["params"].as_object().expect("params field");
            let catalog_keys: HashSet<&str> = params.keys().map(String::as_str).collect();
            let known: HashSet<&str> = effect_param_keys(kind)
                .unwrap_or_else(|| panic!("effect_param_keys missing entry for {kind:?}"))
                .iter()
                .copied()
                .collect();
            assert_eq!(
                catalog_keys, known,
                "params mismatch for {kind:?}: catalog={catalog_keys:?} vs validate={known:?}"
            );
        }
    }

    #[test]
    fn soundfont_entry_lists_all_drum_keys() {
        // schema 0.2 では SF2 catalog の `drum_keys` フィールドが GM Drum (bank=128) の
        // 要素名キー一覧の正本 (= 旧 drum_kit catalog から CDT-7 で移管)。
        let catalog = instrument_catalog();
        let sf = catalog
            .iter()
            .find(|e| e["type"].as_str() == Some("soundfont"))
            .expect("soundfont catalog entry");
        let keys: Vec<&str> = sf["drum_keys"]
            .as_array()
            .expect("drum_keys array")
            .iter()
            .map(|v| v.as_str().expect("drum key string"))
            .collect();
        for k in KNOWN_DRUM_KEYS {
            assert!(keys.contains(k), "soundfont catalog missing drum key {k}");
        }
    }

    #[test]
    fn schema_has_expected_top_level_defs() {
        let schema = song_json_schema();
        let defs = schema["$defs"].as_object().expect("schema $defs");
        for required in ["Metadata", "Track", "Instrument", "Effect", "Note", "Pitch"] {
            assert!(
                defs.contains_key(required),
                "schema missing $defs.{required}"
            );
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
