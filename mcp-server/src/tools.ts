import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { z } from "zod";

import { runCliAsToolResult } from "./cli.js";
import { resolveSongPath } from "./workspace.js";

/**
 * MCP tool ハンドラの戻り値形状。
 * `content` に JSON を string として詰める (MCP 仕様: text/image/resource のみ受理)。
 *
 * CLI からのレスポンスは常に `{ ok, ... }` 形式の object なので、
 * `structuredContent` にも同じ object を載せて構造化アクセスを可能にする。
 * `ok: false` のときは `isError: true` を立てる (MCP クライアントが
 * エラー扱いできるように)。
 */
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
 * Phase 0 で expose する 5 tool を MCP server に登録する。
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
        overwrite: z
          .boolean()
          .optional()
          .describe("既存ファイルを上書き (default false)"),
      },
    },
    async (input) => {
      const path = resolveSongPath(input.path);
      const args = ["new", path];
      if (input.name !== undefined) args.push("--name", input.name);
      if (input.bpm !== undefined) args.push("--bpm", String(input.bpm));
      if (input.key !== undefined) args.push("--key", input.key);
      if (input.time_signature !== undefined) {
        const [n, d] = input.time_signature;
        args.push("--time-sig", `${n}/${d}`);
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
      const args = [
        "set-notes",
        path,
        "--track",
        input.track_id,
        "--notes-json",
        JSON.stringify(input.notes),
      ];
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

      const args = ["render", songPath, "--output", outPath];
      if (input.sample_rate !== undefined) {
        args.push("--sample-rate", String(input.sample_rate));
      }
      const result = await runCliAsToolResult(args);
      return jsonContent(result);
    },
  );
}
