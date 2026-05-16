#!/usr/bin/env node
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";

import { registerTools } from "./tools.js";
import { registerResources } from "./resources.js";
import { getCodettaBin } from "./cli.js";
import { getWorkspace } from "./workspace.js";

/**
 * Codetta MCP server — stdio エントリポイント。
 *
 * 設計: docs/design/04-mcp.md
 * - stdout は MCP プロトコル占有 (絶対に他のものを書かない)
 * - 人間向けログは stderr へ
 */
async function main(): Promise<void> {
  const server = new McpServer(
    {
      name: "codetta",
      version: "0.0.1",
    },
    {
      capabilities: {
        tools: {},
        resources: { listChanged: false },
      },
      instructions:
        "Codetta は LLM ネイティブな軽量 DAW。list_instruments / list_effects で使える音源を確認し、create_song で .codetta を作り、set_notes でノートを書き込み、render_wav で WAV を出力する。プリセット例は resources の codetta://presets/{name} で参照できる。",
    },
  );

  registerTools(server);
  registerResources(server);

  // 起動時のサニティチェック (stderr のみ)
  process.stderr.write(
    `[codetta-mcp] starting (bin=${getCodettaBin()}, workspace=${getWorkspace()})\n`,
  );

  const transport = new StdioServerTransport();
  await server.connect(transport);

  process.stderr.write("[codetta-mcp] connected (stdio)\n");
}

main().catch((err: unknown) => {
  const msg = err instanceof Error ? err.stack ?? err.message : String(err);
  process.stderr.write(`[codetta-mcp] fatal: ${msg}\n`);
  process.exit(1);
});
