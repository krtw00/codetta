import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { readdir, stat } from "node:fs/promises";
import { join } from "node:path";
import { z } from "zod";

import { runCliAsToolResult } from "./cli.js";
import { getWorkspace, resolveSongPath } from "./workspace.js";

/**
 * MCP tool ハンドラの戻り値形状。
 * `content` に JSON を string として詰める (MCP 仕様: text/image/resource のみ受理)。
 *
 * CLI からのレスポンスは常に `{ ok, ... }` 形式の object なので、
 * `structuredContent` にも同じ object を載せて構造化アクセスを可能にする。
 * `ok: false` のときは `isError: true` を立てる (MCP クライアントが
 * エラー扱いできるように)。
 */
/**
 * `args.push(flag, value)` の代わりに使う helper。
 *
 * `value` が `-` で始まる場合は clap が `-0` 等のフラグと誤認するので
 * `--flag=value` 形式 1 引数で渡す。負の数値 (pan: -0.2 等) を CLI に
 * 安全に渡すのが主な動機。
 */
function pushOpt(args: string[], flag: string, value: string): void {
  if (value.startsWith("-")) {
    args.push(`${flag}=${value}`);
  } else {
    args.push(flag, value);
  }
}

function jsonContent(value: unknown): {
  content: { type: "text"; text: string }[];
  structuredContent?: { [x: string]: unknown };
  isError?: boolean;
} {
  const text = JSON.stringify(value, null, 2);
  const isObject = typeof value === "object" && value !== null;
  const isErr =
    isObject && (value as { ok?: unknown }).ok === false;
  return {
    content: [{ type: "text", text }],
    ...(isObject
      ? { structuredContent: value as { [x: string]: unknown } }
      : {}),
    isError: isErr,
  };
}

/**
 * Phase 1 で expose する全 tool を MCP server に登録する。
 * 設計: docs/design/04-mcp.md
 *
 * `path` の解釈: 絶対パスはそのまま、相対パスは `$CODETTA_WORKSPACE` 配下。
 * 詳細は workspace.ts 参照。
 */
export function registerTools(server: McpServer): void {
  // ----- list_instruments -----
  server.registerTool(
    "list_instruments",
    {
      title: "List available instruments",
      description:
        "Codetta が内蔵する楽器一覧と各 type のパラメータスキーマを返す。新しいトラックを add_track する前に呼ぶことを推奨。",
      inputSchema: {},
    },
    async () => {
      const result = await runCliAsToolResult(["list-instruments"]);
      return jsonContent(result);
    },
  );

  // ----- list_effects -----
  server.registerTool(
    "list_effects",
    {
      title: "List available effects",
      description:
        "Codetta が内蔵するエフェクト一覧と各 type のパラメータスキーマを返す。set_fx の入力を決める前に呼ぶ。",
      inputSchema: {},
    },
    async () => {
      const result = await runCliAsToolResult(["list-effects"]);
      return jsonContent(result);
    },
  );

  // ----- list_soundfont_presets -----
  server.registerTool(
    "list_soundfont_presets",
    {
      title: "List presets in a SoundFont (.sf2)",
      description:
        "指定 SF2 ファイルに含まれる preset 一覧 (bank / preset / name) と SF2 メタ情報を返す。soundfont 楽器の `preset` / `bank` 値を決める前に使う。file は絶対パスか $CODETTA_SOUNDFONT_DIR (default ~/Music/sf2/) 配下の相対パス。",
      inputSchema: {
        file: z
          .string()
          .describe(
            "SF2 ファイル path。絶対 or $CODETTA_SOUNDFONT_DIR 配下の相対",
          ),
      },
    },
    async (input) => {
      const args = ["list-soundfont-presets", input.file];
      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );

  // ----- create_song -----
  server.registerTool(
    "create_song",
    {
      title: "Create a new Codetta song",
      description:
        "新規プロジェクト (.codetta) を作成する。path は絶対 / 相対 (workspace 配下) のどちらでも可。time_signature は [N, D] 2 要素配列。",
      inputSchema: {
        path: z
          .string()
          .describe("出力ファイルパス。相対パスなら $CODETTA_WORKSPACE 配下"),
        name: z
          .string()
          .optional()
          .describe("楽曲名 (省略時は path の stem を使う)"),
        bpm: z
          .number()
          .int()
          .min(20)
          .max(400)
          .optional()
          .describe("BPM (default 120)"),
        key: z.string().optional().describe("調 (例: 'Am', 'C', default 'C')"),
        time_signature: z
          .tuple([z.number().int().positive(), z.number().int().positive()])
          .optional()
          .describe("拍子 [N, D] (default [4, 4])"),
        master_gain: z
          .number()
          .min(0)
          .max(4)
          .optional()
          .describe(
            "全 track 合算後 (soft_clip 前) に乗算する master gain。 0.0-4.0 (default 1.0)。 voice 密度別の目安: 単 track or 薄いアレンジ=2.0 / 2 track 中音域=1.5-1.8 / 3 track 以上で chord pad + percussion 同時発音=1.0-1.2 (peak overflow + soft_clip の歪み回避)。 不明なら render 後に試聴して調整する。",
          ),
        overwrite: z
          .boolean()
          .optional()
          .describe("既存ファイルを上書き (default false)"),
      },
    },
    async (input) => {
      const path = resolveSongPath(input.path);
      const args = ["new", path];
      if (input.name !== undefined) pushOpt(args, "--name", input.name);
      if (input.bpm !== undefined) pushOpt(args, "--bpm", String(input.bpm));
      if (input.key !== undefined) pushOpt(args, "--key", input.key);
      if (input.time_signature !== undefined) {
        const [n, d] = input.time_signature;
        pushOpt(args, "--time-sig", `${n}/${d}`);
      }
      if (input.master_gain !== undefined) {
        pushOpt(args, "--master-gain", String(input.master_gain));
      }
      if (input.overwrite === true) args.push("--force");

      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );

  // ----- set_notes -----
  server.registerTool(
    "set_notes",
    {
      title: "Replace notes on a track",
      description:
        "指定トラックのノート列を全置換する (冪等)。notes は { t, pitch, dur, vel } の配列。pitch は 'A4' 等の文字列 or MIDI 番号、drum_kit の場合は 'kick' 等のドラムキー。",
      inputSchema: {
        path: z.string().describe("対象 .codetta ファイル"),
        track_id: z.string().describe("ノートを書き込むトラック ID"),
        notes: z
          .array(
            z.object({
              t: z.number().describe("開始ビート"),
              pitch: z
                .union([z.string(), z.number().int()])
                .describe("音高 (例: 'A4', 60, 'kick')"),
              dur: z.number().positive().describe("長さ (ビート)"),
              vel: z
                .number()
                .int()
                .min(0)
                .max(127)
                .describe("ベロシティ 0-127"),
            }),
          )
          .describe("置換するノート列"),
      },
    },
    async (input) => {
      const path = resolveSongPath(input.path);
      const args = ["set-notes", path];
      pushOpt(args, "--track", input.track_id);
      pushOpt(args, "--notes-json", JSON.stringify(input.notes));
      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );

  // ----- render_wav -----
  server.registerTool(
    "render_wav",
    {
      title: "Render the song to a WAV file",
      description:
        "プロジェクトを WAV にレンダリングする。output を省略すると <path>.wav に書き出す。LLM への注意: WAV バイト列は返さない (path のみ)。ユーザーが afplay 等で再生する。",
      inputSchema: {
        path: z.string().describe("入力 .codetta ファイル"),
        output: z
          .string()
          .optional()
          .describe("出力 WAV パス (省略時 <path>.wav)"),
        sample_rate: z
          .union([z.literal(44100), z.literal(48000)])
          .optional()
          .describe("サンプルレート (default 44100)"),
      },
    },
    async (input) => {
      const songPath = resolveSongPath(input.path);
      const outPath =
        input.output !== undefined
          ? resolveSongPath(input.output)
          : songPath.replace(/\.codetta$/, "") + ".wav";

      const args = ["render", songPath];
      pushOpt(args, "--output", outPath);
      if (input.sample_rate !== undefined) {
        pushOpt(args, "--sample-rate", String(input.sample_rate));
      }
      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );

  // ----- add_track -----
  server.registerTool(
    "add_track",
    {
      title: "Add a new track to a song",
      description:
        "既存プロジェクトに新規トラックを追加する。instrument は list_instruments で確認できる type 名。params は Instrument 固有のパラメータ (例: { attack: 0.02 })。",
      inputSchema: {
        path: z.string().describe("対象 .codetta ファイル"),
        track_id: z
          .string()
          .describe("トラック ID (kebab-case 推奨、 song 内 unique)"),
        name: z
          .string()
          .optional()
          .describe("表示名 (省略時は track_id と同じ)"),
        instrument: z
          .string()
          .optional()
          .describe("楽器 type (例: 'sin', 'saw_lead', 'drum_kit'。 default 'sin')"),
        volume: z
          .number()
          .min(0)
          .max(1)
          .optional()
          .describe("音量 0.0-1.0 (default 0.8)"),
        pan: z
          .number()
          .min(-1)
          .max(1)
          .optional()
          .describe("パン -1.0 (L) 〜 1.0 (R) (default 0.0)"),
        params: z
          .record(z.string(), z.unknown())
          .optional()
          .describe("楽器固有パラメータの object (例: { attack: 0.02 })"),
      },
    },
    async (input) => {
      const path = resolveSongPath(input.path);
      const args = ["add-track", path];
      pushOpt(args, "--id", input.track_id);
      if (input.name !== undefined) pushOpt(args, "--name", input.name);
      if (input.instrument !== undefined)
        pushOpt(args, "--instrument", input.instrument);
      if (input.volume !== undefined)
        pushOpt(args, "--volume", String(input.volume));
      if (input.pan !== undefined) pushOpt(args, "--pan", String(input.pan));
      if (input.params !== undefined)
        pushOpt(args, "--params-json", JSON.stringify(input.params));

      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );

  // ----- get_song -----
  server.registerTool(
    "get_song",
    {
      title: "Get song metadata + track summary",
      description:
        "プロジェクトの metadata / トラック一覧 (id, name, instrument, note_count, fx_count) / 演奏長を返す。トラックの全ノートは返さない (大きすぎるため) — ノート詳細が必要なら resource codetta://songs/{path} を読む。",
      inputSchema: {
        path: z.string().describe("対象 .codetta ファイル"),
      },
    },
    async (input) => {
      const path = resolveSongPath(input.path);
      const result = await runCliAsToolResult(["info", path]);
      return jsonContent(result);
    },
  );

  // ----- validate_song -----
  server.registerTool(
    "validate_song",
    {
      title: "Validate a song",
      description:
        "スキーマ + 整合性検証を実行する。エラーがあれば { ok: false, errors: [{code, message, ...}] } を返す。render_wav は内部で validate するので、書き換え後は render 前にこれで先に確認するとデバッグしやすい。",
      inputSchema: {
        path: z.string().describe("対象 .codetta ファイル"),
      },
    },
    async (input) => {
      const path = resolveSongPath(input.path);
      const result = await runCliAsToolResult(["validate", path]);
      return jsonContent(result);
    },
  );

  // ----- migrate_song -----
  server.registerTool(
    "migrate_song",
    {
      title: "Migrate a 0.1 song to 0.2 (internal synth → SF2)",
      description:
        "schema 0.1 の .codetta を 0.2 に変換する。内蔵 synth type (sin / saw / saw_lead / square / square_bass / triangle / saw_pad / drum_kit) を LUT で SF2 (`soundfont` type) preset/bank に置き換え、 元の params (ADSR / pulse_width / kit 等) は破棄する。LUT 未掲載 type は preset 0 (Acoustic Grand Piano) にフォールバックし MIGRATE_UNKNOWN_INSTRUMENT 警告を返す。 `soundfont` 既存 track は no-op。 output 省略時は `<stem>-0.2.codetta` を同一ディレクトリに書き出す (input 上書きはユーザー意図的な指定が必要)。 出力 file の妥当性検査は別途 validate_song を呼ぶこと。",
      inputSchema: {
        path: z.string().describe("入力 .codetta ファイル (schema 0.1)"),
        output: z
          .string()
          .optional()
          .describe("出力 .codetta path (省略時 <stem>-0.2.codetta)"),
        sf2_file: z
          .string()
          .optional()
          .describe(
            "LUT 適用後の soundfont params.file に書き込む SF2 ファイル名。省略時は default (GeneralUser-GS-v1.471.sf2)",
          ),
      },
    },
    async (input) => {
      const inPath = resolveSongPath(input.path);
      const outPath =
        input.output !== undefined
          ? resolveSongPath(input.output)
          : inPath.replace(/\.codetta$/, "") + "-0.2.codetta";

      const args = ["migrate", inPath, "-o", outPath];
      if (input.sf2_file !== undefined) {
        pushOpt(args, "--sf2", input.sf2_file);
      }
      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );

  // ----- import_midi -----
  server.registerTool(
    "import_midi",
    {
      title: "Import a standard MIDI file (.mid) into .codetta (0.2)",
      description:
        "標準 MIDI ファイルを `.codetta` (schema 0.2) に取り込む。 SMF Type 0 / Type 1 両対応 (Type 2 と SMPTE timecode は reject)。 channel ↔ track は 1:1 (ch10 = drum bank 128)、 ノート pitch は数値固定 (drum の要素名キー逆変換は CDT-3 では未実装)。 拡張属性 (master_gain / fx / SF2 preset 詳細) は `extensions` モードに従って復元: `text-meta` (default、 MTrk 0 先頭の Text Meta) → sidecar (`<basename>.codetta.meta.json`) → MIDI のみ。 `sf2` 指定時は preset 存在確認を行い、 見つからなければ preset 0 fallback + warning。 ADR: docs/design/08-midi.md",
      inputSchema: {
        path: z
          .string()
          .describe("入力 .mid ファイル。 相対パスなら $CODETTA_WORKSPACE 配下"),
        output: z
          .string()
          .describe("出力 .codetta ファイル。 相対パスなら $CODETTA_WORKSPACE 配下"),
        extensions: z
          .enum(["text-meta", "sidecar", "none"])
          .optional()
          .describe(
            "拡張属性の取り出しモード (default: 'text-meta')",
          ),
        sf2_file: z
          .string()
          .optional()
          .describe(
            "Instrument.params.file に書く SF2 ファイル名 (省略時は default SF2 = GeneralUser-GS-v1.471.sf2)。 指定があれば preset 存在確認も行う",
          ),
        name: z
          .string()
          .optional()
          .describe("生成 Song の metadata.name (省略時は MIDI path の stem)"),
        overwrite: z
          .boolean()
          .optional()
          .describe("既存 .codetta を上書き (default false)"),
      },
    },
    async (input) => {
      const inPath = resolveSongPath(input.path);
      const outPath = resolveSongPath(input.output);
      const args = ["import-midi", inPath, "-o", outPath];
      if (input.extensions !== undefined) {
        pushOpt(args, "--extensions", input.extensions);
      }
      if (input.sf2_file !== undefined) {
        pushOpt(args, "--sf2", input.sf2_file);
      }
      if (input.name !== undefined) {
        pushOpt(args, "--name", input.name);
      }
      if (input.overwrite === true) {
        args.push("--force");
      }
      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );

  // ----- export_midi -----
  server.registerTool(
    "export_midi",
    {
      title: "Export a .codetta to standard MIDI (.mid, SMF Type 1)",
      description:
        "`.codetta` を SMF Type 1 (multi-track) として書き出す。 channel ↔ track は 1:1、 drum (bank=128) は ch10 固定。 melodic は最大 15 track、 drum は 1 track のみ (超過は MIDI_TRACK_LIMIT_EXCEEDED / MIDI_MULTIPLE_DRUM_TRACKS エラー)。 拡張属性 (master_gain / fx / SF2 preset 詳細) は `extensions` モードで書き出す: `text-meta` (default、 MTrk 0 先頭の Text Meta に JSON inline) / `sidecar` (`<basename>.codetta.meta.json`) / `none` (純粋 GM 互換)。 0.1 入力は in-memory migrate で 0.2 化してから export (= 結果は `implicit_migrate` field に summary)。 ADR: docs/design/08-midi.md",
      inputSchema: {
        path: z
          .string()
          .describe("入力 .codetta ファイル。 相対パスなら $CODETTA_WORKSPACE 配下"),
        output: z
          .string()
          .describe("出力 .mid ファイル。 相対パスなら $CODETTA_WORKSPACE 配下"),
        extensions: z
          .enum(["text-meta", "sidecar", "none"])
          .optional()
          .describe(
            "拡張属性の書き出しモード (default: 'text-meta')",
          ),
        ppq: z
          .number()
          .int()
          .min(1)
          .max(32767)
          .optional()
          .describe("PPQ (ticks per quarter)。 default 480 (ADR L43)"),
        sf2_file: z
          .string()
          .optional()
          .describe(
            "0.1 入力の in-memory migrate 時に soundfont params.file に書き込む SF2 ファイル名 (省略時は default SF2 = GeneralUser-GS-v1.471.sf2)。 0.2 入力では使用されない",
          ),
        overwrite: z
          .boolean()
          .optional()
          .describe("既存 .mid を上書き (default false)"),
      },
    },
    async (input) => {
      const inPath = resolveSongPath(input.path);
      const outPath = resolveSongPath(input.output);
      const args = ["export-midi", inPath, "-o", outPath];
      if (input.extensions !== undefined) {
        pushOpt(args, "--extensions", input.extensions);
      }
      if (input.ppq !== undefined) {
        pushOpt(args, "--ppq", String(input.ppq));
      }
      if (input.sf2_file !== undefined) {
        pushOpt(args, "--sf2", input.sf2_file);
      }
      if (input.overwrite === true) {
        args.push("--force");
      }
      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );

  // ----- set_instrument -----
  server.registerTool(
    "set_instrument",
    {
      title: "Replace a track's instrument",
      description:
        "指定トラックの楽器 (type + params) を完全置換する。params は Instrument 固有のパラメータ object (例: lowpass cutoff など)。 list_instruments で各 type のパラメータスキーマを確認できる。",
      inputSchema: {
        path: z.string().describe("対象 .codetta ファイル"),
        track_id: z.string().describe("対象トラック ID"),
        type: z.string().describe("新しい楽器 type (例: 'saw_lead', 'drum_kit')"),
        params: z
          .record(z.string(), z.unknown())
          .optional()
          .describe("楽器固有パラメータの object (省略時は空)"),
      },
    },
    async (input) => {
      const path = resolveSongPath(input.path);
      const args = ["set-instrument", path];
      pushOpt(args, "--track", input.track_id);
      pushOpt(args, "--type", input.type);
      if (input.params !== undefined)
        pushOpt(args, "--params-json", JSON.stringify(input.params));

      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );

  // ----- set_fx -----
  server.registerTool(
    "set_fx",
    {
      title: "Replace a track's FX chain",
      description:
        "指定トラックの fx チェーン (= エフェクト配列) を全置換する。fx は [{type, ...params}, ...] 形式。空配列を渡すと fx をクリア。list_effects で各 effect type のパラメータスキーマを確認できる。",
      inputSchema: {
        path: z.string().describe("対象 .codetta ファイル"),
        track_id: z.string().describe("対象トラック ID"),
        fx: z
          .array(z.object({ type: z.string() }).passthrough())
          .describe("置換する fx チェーン (例: [{ type: 'lowpass', cutoff: 1200 }])"),
      },
    },
    async (input) => {
      const path = resolveSongPath(input.path);
      const args = ["set-fx", path];
      pushOpt(args, "--track", input.track_id);
      pushOpt(args, "--fx-json", JSON.stringify(input.fx));
      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );

  // ----- set_master_gain -----
  server.registerTool(
    "set_master_gain",
    {
      title: "Set the project's master gain",
      description:
        "プロジェクトの metadata.master_gain を変更する。 0.0-4.0、 default 1.0。 全 track 合算後 (soft_clip 前) に乗算される post-mix gain。 voice 密度別の目安: 単 track or 薄いアレンジ=2.0 / 2 track 中音域=1.5-1.8 / 3 track 以上で chord pad + percussion 同時発音=1.0-1.2 (peak overflow + soft_clip の歪み回避)。 不明なら render 後に試聴して調整する。",
      inputSchema: {
        path: z.string().describe("対象 .codetta ファイル"),
        value: z
          .number()
          .min(0)
          .max(4)
          .describe("master gain (0.0-4.0)"),
      },
    },
    async (input) => {
      const path = resolveSongPath(input.path);
      const args = ["set-master-gain", path];
      pushOpt(args, "--value", String(input.value));
      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );

  // ----- edit_notes -----
  server.registerTool(
    "edit_notes",
    {
      title: "Apply bulk note operations on a track",
      description:
        "指定トラックのノート列に対する一括変形 op を順次適用する。op は { op: '<name>', ... } 形式 (例: { op: 'transpose', semitones: 12 }, { op: 'shift', delta: 0.5 }, { op: 'scale', factor: 0.5 }, { op: 'quantize', grid: 0.25 })。set_notes で全置換するより diff 編集に向く。",
      inputSchema: {
        path: z.string().describe("対象 .codetta ファイル"),
        track_id: z.string().describe("対象トラック ID"),
        ops: z
          .array(z.object({ op: z.string() }).passthrough())
          .describe("適用する操作配列 (順次実行)"),
      },
    },
    async (input) => {
      const path = resolveSongPath(input.path);
      const args = ["edit-notes", path];
      pushOpt(args, "--track", input.track_id);
      pushOpt(args, "--ops-json", JSON.stringify(input.ops));
      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );

  // ----- add_notes -----
  server.registerTool(
    "add_notes",
    {
      title: "Append notes to a track",
      description:
        "指定トラックにノートを追加する (既存ノートを保持。 完全一致は skipped_duplicates に計上)。 set_notes と違い差分追記に向く。",
      inputSchema: {
        path: z.string().describe("対象 .codetta ファイル"),
        track_id: z.string().describe("ノートを追加するトラック ID"),
        notes: z
          .array(
            z.object({
              t: z.number().describe("開始ビート"),
              pitch: z
                .union([z.string(), z.number().int()])
                .describe("音高 (例: 'A4', 60, 'kick')"),
              dur: z.number().positive().describe("長さ (ビート)"),
              vel: z
                .number()
                .int()
                .min(0)
                .max(127)
                .describe("ベロシティ 0-127"),
            }),
          )
          .describe("追加するノート列"),
      },
    },
    async (input) => {
      const path = resolveSongPath(input.path);
      const args = ["add-notes", path];
      pushOpt(args, "--track", input.track_id);
      pushOpt(args, "--notes-json", JSON.stringify(input.notes));
      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );

  // ----- clear_notes -----
  server.registerTool(
    "clear_notes",
    {
      title: "Clear all notes on a track",
      description: "指定トラックのノートを全削除する。トラック自体は残る (set_notes に空配列を渡すのと等価)。",
      inputSchema: {
        path: z.string().describe("対象 .codetta ファイル"),
        track_id: z.string().describe("対象トラック ID"),
      },
    },
    async (input) => {
      const path = resolveSongPath(input.path);
      const args = ["clear-notes", path];
      pushOpt(args, "--track", input.track_id);
      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );

  // ----- remove_track -----
  server.registerTool(
    "remove_track",
    {
      title: "Remove a track from a song",
      description: "指定トラックを song から削除する (ノート / fx ごと)。 取り消し不可。",
      inputSchema: {
        path: z.string().describe("対象 .codetta ファイル"),
        track_id: z.string().describe("削除するトラック ID"),
      },
    },
    async (input) => {
      const path = resolveSongPath(input.path);
      const args = ["remove-track", path];
      pushOpt(args, "--id", input.track_id);
      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );

  // ----- list_songs -----
  server.registerTool(
    "list_songs",
    {
      title: "List .codetta files in the workspace",
      description:
        "$CODETTA_WORKSPACE 配下の .codetta ファイルを列挙する (再帰なし)。 各 entry は { name, path, size_bytes, modified } を含む。 CLI を経由せず Node の fs で読むので codetta バイナリ不在でも動く。",
      inputSchema: {},
    },
    async () => {
      const ws = getWorkspace();
      let entries: string[];
      try {
        entries = await readdir(ws);
      } catch (err) {
        return jsonContent({
          ok: false,
          error: {
            code: "WORKSPACE_READ_FAILED",
            message: `Failed to read workspace: ${(err as Error).message}`,
            hint: "$CODETTA_WORKSPACE のパス権限を確認する",
            context: { workspace: ws },
          },
        });
      }

      const songs: Array<{
        name: string;
        path: string;
        size_bytes: number;
        modified: string;
      }> = [];
      for (const entry of entries) {
        if (!entry.endsWith(".codetta")) continue;
        const full = join(ws, entry);
        try {
          const st = await stat(full);
          if (!st.isFile()) continue;
          songs.push({
            name: entry.replace(/\.codetta$/, ""),
            path: full,
            size_bytes: st.size,
            modified: st.mtime.toISOString(),
          });
        } catch {
          // 列挙中に消えた等のレースは無視
        }
      }
      songs.sort((a, b) => a.name.localeCompare(b.name));

      return jsonContent({
        ok: true,
        workspace: ws,
        songs,
      });
    },
  );
}
