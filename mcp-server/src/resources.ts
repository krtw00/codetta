import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { ResourceTemplate } from "@modelcontextprotocol/sdk/server/mcp.js";
import { readFile, readdir, stat } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve, basename } from "node:path";

/**
 * `docs/examples/` の .codetta プリセットを MCP resources として公開する。
 *
 * URI scheme: `codetta://presets/{name}` (拡張子なし)
 * 例: `codetta://presets/cyber-lead` -> docs/examples/cyber-lead.codetta
 *
 * 解決順:
 * 1. 環境変数 `CODETTA_PRESETS_DIR` (絶対パス)
 * 2. このファイルからの相対 (`<mcp-server>/dist/resources.js` を起点に `../../docs/examples`)
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

export function registerResources(server: McpServer): void {
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
}
