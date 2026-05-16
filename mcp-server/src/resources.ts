import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { ResourceTemplate } from "@modelcontextprotocol/sdk/server/mcp.js";
import { readFile, readdir, stat } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve, basename } from "node:path";

import { runCli } from "./cli.js";
import { getWorkspace } from "./workspace.js";

/**
 * MCP resources の登録。
 *
 * 設計: docs/design/04-mcp.md「Resources」
 *
 * URI scheme 一覧:
 * - `codetta://presets/{name}`           — docs/examples/{name}.codetta
 * - `codetta://instruments`              — list-instruments CLI と同内容
 * - `codetta://effects`                  — list-effects CLI と同内容
 * - `codetta://schema/song/{version}`    — schema CLI と同内容 (version は schema_version と一致必須)
 * - `codetta://songs/{name}`             — $CODETTA_WORKSPACE/{name}.codetta の生 JSON
 *
 * 設計上の選択:
 * - resources は「LLM が context として読む静的データ」。 CLI 由来でも `{ok:true, ...}`
 *   wrapper は剥がして payload だけ返す (presets が .codetta 生 JSON を返すのと同じ)。
 * - tools が同等の機能 (list_instruments / list_effects / get_song) を提供するのは意図的:
 *   tools は agent 主導の単発呼び出し、 resources は ` @codetta:instruments` 等で
 *   context として持ち込む用途を想定 (04-mcp.md)。
 */

function getPresetsDir(): string {
  const env = process.env.CODETTA_PRESETS_DIR;
  if (env && env.length > 0) return resolve(env);
  // dist/resources.js -> dist/ -> mcp-server/ -> <repo root>/ -> docs/examples
  const here = dirname(fileURLToPath(import.meta.url));
  return resolve(here, "..", "..", "docs", "examples");
}

async function listPresets(): Promise<{ name: string; path: string }[]> {
  const dir = getPresetsDir();
  let entries: string[];
  try {
    entries = await readdir(dir);
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === "ENOENT") {
      return [];
    }
    throw err;
  }
  const presets: { name: string; path: string }[] = [];
  for (const entry of entries) {
    if (!entry.endsWith(".codetta")) continue;
    const full = join(dir, entry);
    const st = await stat(full);
    if (!st.isFile()) continue;
    presets.push({ name: entry.replace(/\.codetta$/, ""), path: full });
  }
  presets.sort((a, b) => a.name.localeCompare(b.name));
  return presets;
}

async function listWorkspaceSongs(): Promise<{ name: string; path: string }[]> {
  const ws = getWorkspace();
  let entries: string[];
  try {
    entries = await readdir(ws);
  } catch {
    return [];
  }
  const songs: { name: string; path: string }[] = [];
  for (const entry of entries) {
    if (!entry.endsWith(".codetta")) continue;
    const full = join(ws, entry);
    try {
      const st = await stat(full);
      if (!st.isFile()) continue;
      songs.push({ name: entry.replace(/\.codetta$/, ""), path: full });
    } catch {
      // 列挙中に消えた等のレースは無視
    }
  }
  songs.sort((a, b) => a.name.localeCompare(b.name));
  return songs;
}

/**
 * CLI を呼んで JSON を取り、 `ok:true` の場合 wrapper を剥がして payload だけ返す。
 * 失敗時は Error を投げて MCP の resource read エラーにする。
 */
async function readCliJson(args: string[]): Promise<Record<string, unknown>> {
  const result = await runCli(args);
  if (result.exitCode !== 0 || result.json === null) {
    const stderrTail = result.stderr.slice(-512);
    throw new Error(
      `codetta-cli failed (args=${JSON.stringify(args)}, exit=${result.exitCode}): ${stderrTail}`,
    );
  }
  const obj = result.json as Record<string, unknown>;
  if (obj.ok !== true) {
    throw new Error(
      `codetta-cli returned ok=false (args=${JSON.stringify(args)}): ${JSON.stringify(obj).slice(0, 512)}`,
    );
  }
  // ok wrapper を剥がした残りを返す
  const { ok: _ok, ...rest } = obj;
  return rest;
}

export function registerResources(server: McpServer): void {
  // ----- codetta://presets/{name} (既存) -----
  server.registerResource(
    "presets",
    new ResourceTemplate("codetta://presets/{name}", {
      list: async () => {
        const presets = await listPresets();
        return {
          resources: presets.map((p) => ({
            uri: `codetta://presets/${p.name}`,
            name: p.name,
            description: `Codetta preset song (.codetta JSON) — source: ${basename(p.path)}`,
            mimeType: "application/json",
          })),
        };
      },
    }),
    {
      title: "Codetta example presets",
      description:
        "docs/examples/ 配下のサンプル .codetta を読み取り専用 resource として公開。LLM が「テンプレ見せて」 と要求した時の参照元。",
      mimeType: "application/json",
    },
    async (uri, variables) => {
      const rawName = variables.name;
      const name = Array.isArray(rawName) ? rawName[0] : rawName;
      if (typeof name !== "string" || name.length === 0) {
        throw new Error(`Invalid preset URI: ${uri.href}`);
      }
      if (name.includes("/") || name.includes("..")) {
        throw new Error(`Invalid preset name: ${name}`);
      }
      const path = join(getPresetsDir(), `${name}.codetta`);
      const text = await readFile(path, "utf8");
      return {
        contents: [
          {
            uri: uri.href,
            mimeType: "application/json",
            text,
          },
        ],
      };
    },
  );

  // ----- codetta://instruments -----
  server.registerResource(
    "instruments",
    "codetta://instruments",
    {
      title: "Codetta instrument catalog",
      description:
        "内蔵楽器 (Instrument type) の一覧とパラメータスキーマ。list_instruments tool と同内容を読み取り専用 resource として公開。",
      mimeType: "application/json",
    },
    async (uri) => {
      const payload = await readCliJson(["list-instruments"]);
      return {
        contents: [
          {
            uri: uri.href,
            mimeType: "application/json",
            text: JSON.stringify(payload, null, 2),
          },
        ],
      };
    },
  );

  // ----- codetta://effects -----
  server.registerResource(
    "effects",
    "codetta://effects",
    {
      title: "Codetta effect catalog",
      description:
        "内蔵エフェクト (Effect type) の一覧とパラメータスキーマ。list_effects tool と同内容を読み取り専用 resource として公開。",
      mimeType: "application/json",
    },
    async (uri) => {
      const payload = await readCliJson(["list-effects"]);
      return {
        contents: [
          {
            uri: uri.href,
            mimeType: "application/json",
            text: JSON.stringify(payload, null, 2),
          },
        ],
      };
    },
  );

  // ----- codetta://schema/song/{version} -----
  server.registerResource(
    "song_schema",
    new ResourceTemplate("codetta://schema/song/{version}", {
      list: async () => {
        // CLI から現行 schema_version を取って 1 件だけ返す。
        // 将来 multi-version になったら CLI に `--list-versions` を追加する想定。
        try {
          const payload = await readCliJson(["schema"]);
          const v = payload.schema_version;
          if (typeof v !== "string") return { resources: [] };
          return {
            resources: [
              {
                uri: `codetta://schema/song/${v}`,
                name: `song schema v${v}`,
                description: `Codetta project file (.codetta) JSON Schema, version ${v}`,
                mimeType: "application/schema+json",
              },
            ],
          };
        } catch {
          return { resources: [] };
        }
      },
    }),
    {
      title: "Codetta song JSON Schema",
      description:
        "プロジェクトファイル (.codetta) の JSON Schema。schema CLI と同内容。{version} は schema_version (例: '0.1') と一致必須。",
      mimeType: "application/schema+json",
    },
    async (uri, variables) => {
      const rawVer = variables.version;
      const version = Array.isArray(rawVer) ? rawVer[0] : rawVer;
      if (typeof version !== "string" || version.length === 0) {
        throw new Error(`Invalid schema URI: ${uri.href}`);
      }
      const payload = await readCliJson(["schema"]);
      const current = payload.schema_version;
      if (version !== current) {
        throw new Error(
          `Unknown schema version: ${version} (available: ${String(current)})`,
        );
      }
      return {
        contents: [
          {
            uri: uri.href,
            mimeType: "application/schema+json",
            text: JSON.stringify(payload.schema, null, 2),
          },
        ],
      };
    },
  );

  // ----- codetta://songs/{name} -----
  server.registerResource(
    "songs",
    new ResourceTemplate("codetta://songs/{name}", {
      list: async () => {
        const songs = await listWorkspaceSongs();
        return {
          resources: songs.map((s) => ({
            uri: `codetta://songs/${s.name}`,
            name: s.name,
            description: `Workspace song (.codetta) — source: ${basename(s.path)}`,
            mimeType: "application/json",
          })),
        };
      },
    }),
    {
      title: "Codetta workspace songs",
      description:
        "$CODETTA_WORKSPACE 配下の .codetta を読み取り専用 resource として公開。{name} は拡張子なしのベース名 (list_songs tool の name と同じ)。set_notes 等で書き換えた直後に内容を確認するのに使える。",
      mimeType: "application/json",
    },
    async (uri, variables) => {
      const rawName = variables.name;
      const name = Array.isArray(rawName) ? rawName[0] : rawName;
      if (typeof name !== "string" || name.length === 0) {
        throw new Error(`Invalid song URI: ${uri.href}`);
      }
      if (name.includes("/") || name.includes("..")) {
        throw new Error(`Invalid song name: ${name}`);
      }
      const ws = getWorkspace();
      const path = join(ws, `${name}.codetta`);
      const text = await readFile(path, "utf8");
      return {
        contents: [
          {
            uri: uri.href,
            mimeType: "application/json",
            text,
          },
        ],
      };
    },
  );
}
