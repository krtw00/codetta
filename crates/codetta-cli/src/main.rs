//! Codetta CLI entry point.
//!
//! 設計: docs/design/03-cli.md
//!
//! Phase 0 の現スコープ: `new` / `info` / `validate`。
//! `add-track` / `set-notes` / `render` は続く実装で追加する。

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use codetta_core::{self as core, CodettaError};
use serde_json::{json, Value};

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

fn main() -> ExitCode {
    let cli = Cli::parse();
    let exit = match cli.command {
        Command::New(a) => cmd_new(a, &cli.common),
        Command::Info(a) => cmd_info(a, &cli.common),
        Command::Validate(a) => cmd_validate(a, &cli.common),
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
        CodettaError::Validation(errs) => {
            emit_json(&json!({ "ok": false, "errors": errs }));
            return 1;
        }
    };
    emit_json(&json!({
        "ok": false,
        "errors": [{ "code": code, "message": msg }]
    }));
    exit
}
