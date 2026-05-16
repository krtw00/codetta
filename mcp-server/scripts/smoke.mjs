#!/usr/bin/env node
/**
 * Codetta MCP server の handshake smoke test。
 *
 * 1. `node dist/index.js` を spawn
 * 2. MCP プロトコルの initialize / tools/list / resources/list / resources/templates/list を投げる
 * 3. 各レスポンスを assert
 *
 * 期待: 5 tool (list_instruments / list_effects / create_song / set_notes / render_wav) と
 *      1 resource template (codetta://presets/{name}) が列挙される。
 */
import { spawn } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const serverEntry = resolve(__dirname, "..", "dist", "index.js");
const repoRoot = resolve(__dirname, "..", "..");
const codettaBin = resolve(repoRoot, "target", "release", "codetta");

const child = spawn("node", [serverEntry], {
  stdio: ["pipe", "pipe", "pipe"],
  env: {
    ...process.env,
    CODETTA_BIN: codettaBin,
    CODETTA_WORKSPACE: resolve(repoRoot, "target", "mcp-smoke-ws"),
  },
});

child.stderr.on("data", (b) => process.stderr.write(`[server] ${b}`));
child.on("error", (e) => {
  console.error("spawn error:", e);
  process.exit(1);
});

const pendingResponses = new Map();
let buf = "";

child.stdout.on("data", (chunk) => {
  buf += chunk.toString("utf8");
  // Content-Length framing は MCP TS SDK の stdio では使われない
  // (newline-delimited JSON)。改行ごとに切り出す。
  let idx;
  while ((idx = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, idx).trim();
    buf = buf.slice(idx + 1);
    if (!line) continue;
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      console.error("invalid json from server:", line);
      continue;
    }
    if (msg.id != null && pendingResponses.has(msg.id)) {
      pendingResponses.get(msg.id).resolve(msg);
      pendingResponses.delete(msg.id);
    } else {
      console.error("server notification:", JSON.stringify(msg));
    }
  }
});

let nextId = 1;
function send(method, params) {
  const id = nextId++;
  const req = { jsonrpc: "2.0", id, method, params };
  return new Promise((resolve, reject) => {
    pendingResponses.set(id, { resolve, reject });
    child.stdin.write(JSON.stringify(req) + "\n");
  });
}
function notify(method, params) {
  child.stdin.write(
    JSON.stringify({ jsonrpc: "2.0", method, params }) + "\n",
  );
}

function assert(cond, msg) {
  if (!cond) {
    console.error("ASSERT FAILED:", msg);
    child.kill();
    process.exit(1);
  }
}

(async () => {
  const initResp = await send("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "smoke", version: "0.0.0" },
  });
  assert(initResp.result?.serverInfo?.name === "codetta", "serverInfo.name");
  console.log("[ok] initialize ->", initResp.result.serverInfo);
  notify("notifications/initialized");

  const tools = await send("tools/list", {});
  const toolNames = (tools.result?.tools ?? []).map((t) => t.name).sort();
  console.log("[ok] tools/list ->", toolNames);
  const expected = [
    "create_song",
    "list_effects",
    "list_instruments",
    "render_wav",
    "set_notes",
  ];
  assert(
    JSON.stringify(toolNames) === JSON.stringify(expected),
    `expected tools ${JSON.stringify(expected)}, got ${JSON.stringify(toolNames)}`,
  );

  const tmpl = await send("resources/templates/list", {});
  const templates = tmpl.result?.resourceTemplates ?? [];
  console.log(
    "[ok] resources/templates/list ->",
    templates.map((t) => t.uriTemplate),
  );
  assert(
    templates.some((t) => t.uriTemplate === "codetta://presets/{name}"),
    "presets template not registered",
  );

  const res = await send("resources/list", {});
  const presets = (res.result?.resources ?? []).map((r) => r.uri).sort();
  console.log("[ok] resources/list ->", presets);
  assert(
    presets.includes("codetta://presets/cyber-lead"),
    "expected preset cyber-lead in resources/list",
  );

  // list_instruments tool call
  const li = await send("tools/call", {
    name: "list_instruments",
    arguments: {},
  });
  assert(!li.result?.isError, "list_instruments returned isError");
  const liStruct = li.result?.structuredContent;
  assert(liStruct?.ok === true, "list_instruments structured.ok !== true");
  assert(
    Array.isArray(liStruct?.instruments) && liStruct.instruments.length > 0,
    "list_instruments.instruments empty",
  );
  console.log("[ok] tools/call list_instruments ->",
    `${liStruct.instruments.length} instruments`);

  // read a preset resource
  const rr = await send("resources/read", {
    uri: "codetta://presets/cyber-lead",
  });
  const contents = rr.result?.contents ?? [];
  assert(contents.length === 1, "expected 1 content");
  assert(
    contents[0].mimeType === "application/json",
    "wrong mimeType",
  );
  const preview = contents[0].text?.slice(0, 60).replace(/\n/g, " ");
  console.log("[ok] resources/read ->", preview, "...");

  // golden path: create_song -> add-track (via CLI directly, not via MCP since
  // add_track tool isn't in skeleton) -> set_notes -> render_wav.
  // ここでは MCP の create_song と set_notes と render_wav だけで完結する
  // 最小フローを叩く。トラック追加は別 tool 化が次フェーズなので、
  // create_song 後に CODETTA_BIN を直接呼んで lead トラックを足す。
  const songName = "smoke-test.codetta";
  const created = await send("tools/call", {
    name: "create_song",
    arguments: {
      path: songName,
      bpm: 130,
      key: "Am",
      name: "MCP Smoke",
      overwrite: true,
    },
  });
  assert(!created.result?.isError, "create_song returned isError");
  assert(
    created.result?.structuredContent?.ok === true,
    "create_song structured.ok !== true",
  );
  const absSongPath = created.result.structuredContent.path;
  console.log("[ok] tools/call create_song ->", absSongPath);

  // add a track via CLI directly (add_track is out of skeleton scope)
  const { execFileSync } = await import("node:child_process");
  execFileSync(
    codettaBin,
    ["add-track", absSongPath, "--id", "lead", "--instrument", "sin"],
    { stdio: "pipe" },
  );

  const setN = await send("tools/call", {
    name: "set_notes",
    arguments: {
      path: songName,
      track_id: "lead",
      notes: [
        { t: 0.0, pitch: "A4", dur: 0.5, vel: 100 },
        { t: 0.5, pitch: "C5", dur: 0.5, vel: 100 },
      ],
    },
  });
  assert(!setN.result?.isError, "set_notes returned isError");
  assert(
    setN.result?.structuredContent?.note_count === 2,
    "set_notes note_count != 2",
  );
  console.log("[ok] tools/call set_notes -> 2 notes");

  const rendered = await send("tools/call", {
    name: "render_wav",
    arguments: { path: songName },
  });
  assert(!rendered.result?.isError, "render_wav returned isError");
  const renderStruct = rendered.result?.structuredContent;
  assert(renderStruct?.ok === true, "render_wav structured.ok !== true");
  assert(
    typeof renderStruct?.output === "string" && renderStruct.output.endsWith(".wav"),
    "render_wav output is not a .wav path",
  );
  console.log(
    "[ok] tools/call render_wav ->",
    renderStruct.output,
    `(${renderStruct.duration_sec}s, ${renderStruct.rtfactor}x rt)`,
  );

  console.log("\nAll smoke checks passed.");
  child.kill();
  process.exit(0);
})().catch((e) => {
  console.error("smoke test failed:", e);
  child.kill();
  process.exit(1);
});
