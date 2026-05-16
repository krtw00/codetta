#!/usr/bin/env node
/**
 * Codetta MCP server の end-to-end smoke test。
 *
 * 1. `node dist/index.js` を spawn
 * 2. MCP プロトコルの initialize / tools/list / resources/list / resources/templates/list
 * 3. **list_soundfont_presets を除く全 tool を最低 1 回叩いて** isError なしを確認
 * 4. golden path (create_song -> add_track -> set_notes -> render_wav) を MCP のみで完結
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

async function callTool(name, args) {
  const resp = await send("tools/call", { name, arguments: args });
  assert(!resp.result?.isError, `${name} returned isError: ${JSON.stringify(resp.result?.structuredContent)}`);
  const struct = resp.result?.structuredContent;
  assert(struct?.ok === true, `${name} structured.ok !== true: ${JSON.stringify(struct)}`);
  return struct;
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
    "add_notes",
    "add_track",
    "clear_notes",
    "create_song",
    "edit_notes",
    "get_song",
    "list_effects",
    "list_instruments",
    "list_songs",
    "list_soundfont_presets",
    "remove_track",
    "render_wav",
    "set_fx",
    "set_instrument",
    "set_master_gain",
    "set_notes",
    "validate_song",
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
  for (const expectedTmpl of [
    "codetta://presets/{name}",
    "codetta://schema/song/{version}",
    "codetta://songs/{name}",
  ]) {
    assert(
      templates.some((t) => t.uriTemplate === expectedTmpl),
      `template ${expectedTmpl} not registered`,
    );
  }

  const res = await send("resources/list", {});
  const resourceUris = (res.result?.resources ?? []).map((r) => r.uri).sort();
  console.log("[ok] resources/list ->", resourceUris);
  for (const expectedUri of [
    "codetta://presets/cyber-lead",
    "codetta://instruments",
    "codetta://effects",
    "codetta://schema/song/0.1",
  ]) {
    assert(
      resourceUris.includes(expectedUri),
      `expected resource ${expectedUri} in resources/list`,
    );
  }

  // catalog tools
  const li = await callTool("list_instruments", {});
  assert(
    Array.isArray(li.instruments) && li.instruments.length > 0,
    "list_instruments empty",
  );
  console.log("[ok] list_instruments ->", `${li.instruments.length} instruments`);

  const le = await callTool("list_effects", {});
  assert(
    Array.isArray(le.effects) && le.effects.length > 0,
    "list_effects empty",
  );
  console.log("[ok] list_effects ->", `${le.effects.length} effects`);

  // resource: presets/cyber-lead
  async function readResource(uri, expectedMime) {
    const r = await send("resources/read", { uri });
    const cs = r.result?.contents ?? [];
    assert(cs.length === 1, `${uri}: expected 1 content`);
    assert(
      cs[0].mimeType === expectedMime,
      `${uri}: mimeType ${cs[0].mimeType} != ${expectedMime}`,
    );
    assert(typeof cs[0].text === "string" && cs[0].text.length > 0, `${uri}: empty text`);
    return cs[0].text;
  }

  const presetText = await readResource(
    "codetta://presets/cyber-lead",
    "application/json",
  );
  console.log("[ok] resources/read presets/cyber-lead ->", presetText.slice(0, 60).replace(/\n/g, " "), "...");

  // resource: instruments / effects (catalog)
  const instrText = await readResource("codetta://instruments", "application/json");
  const instrPayload = JSON.parse(instrText);
  assert(
    Array.isArray(instrPayload.instruments) && instrPayload.instruments.length > 0,
    "instruments resource empty",
  );
  assert(instrPayload.ok === undefined, "instruments resource should not include ok wrapper");
  console.log("[ok] resources/read instruments ->", `${instrPayload.instruments.length} instruments`);

  const fxText = await readResource("codetta://effects", "application/json");
  const fxPayload = JSON.parse(fxText);
  assert(
    Array.isArray(fxPayload.effects) && fxPayload.effects.length > 0,
    "effects resource empty",
  );
  console.log("[ok] resources/read effects ->", `${fxPayload.effects.length} effects`);

  // resource: schema/song/0.1
  const schemaText = await readResource(
    "codetta://schema/song/0.1",
    "application/schema+json",
  );
  const schemaPayload = JSON.parse(schemaText);
  assert(
    typeof schemaPayload.$id === "string" && schemaPayload.$id.includes("song"),
    "schema resource missing $id",
  );
  assert(schemaPayload.title === "Codetta Song", "schema title mismatch");
  console.log("[ok] resources/read schema/song/0.1 ->", schemaPayload.$id);

  // resource: schema with unknown version should fail
  {
    const r = await send("resources/read", { uri: "codetta://schema/song/9.9" });
    assert(r.error != null, "expected error for unknown schema version");
    console.log("[ok] resources/read schema/song/9.9 -> rejected");
  }

  // ----- golden path: MCP のみで完結 -----
  const songName = "smoke-test.codetta";
  const created = await callTool("create_song", {
    path: songName,
    bpm: 130,
    key: "Am",
    name: "MCP Smoke",
    overwrite: true,
  });
  const absSongPath = created.path;
  console.log("[ok] create_song ->", absSongPath);

  // add_track (MCP のみ — CLI 直叩き fallback を撤去)
  const addedLead = await callTool("add_track", {
    path: songName,
    track_id: "lead",
    instrument: "sin",
    volume: 0.8,
  });
  assert(addedLead.track_id === "lead", "add_track track_id mismatch");
  console.log("[ok] add_track lead");

  // 2 本目: drum_kit を params 付きで足す
  const addedDrum = await callTool("add_track", {
    path: songName,
    track_id: "drum",
    instrument: "drum_kit",
    params: { kit: "808" },
  });
  assert(addedDrum.track_id === "drum", "add_track drum mismatch");
  console.log("[ok] add_track drum (with params)");

  // set_notes (lead)
  const setN = await callTool("set_notes", {
    path: songName,
    track_id: "lead",
    notes: [
      { t: 0.0, pitch: "A4", dur: 0.5, vel: 100 },
      { t: 0.5, pitch: "C5", dur: 0.5, vel: 100 },
    ],
  });
  assert(setN.note_count === 2, "set_notes note_count != 2");
  console.log("[ok] set_notes -> 2 notes");

  // add_notes (lead に 2 ノート追加)
  const addN = await callTool("add_notes", {
    path: songName,
    track_id: "lead",
    notes: [
      { t: 1.0, pitch: "E5", dur: 0.5, vel: 100 },
      { t: 1.5, pitch: "G5", dur: 0.5, vel: 100 },
    ],
  });
  assert(addN.added === 2, `add_notes added != 2 (got ${addN.added})`);
  assert(addN.total_notes === 4, `add_notes total != 4 (got ${addN.total_notes})`);
  console.log("[ok] add_notes -> +2 (total 4)");

  // edit_notes (transpose +12)
  const editN = await callTool("edit_notes", {
    path: songName,
    track_id: "lead",
    ops: [{ op: "transpose", semitones: 12 }],
  });
  assert(editN.ops_applied === 1, `edit_notes ops_applied != 1`);
  console.log("[ok] edit_notes -> 1 op applied");

  // drum トラックにノートを書く (kick 4 つ打ち)
  await callTool("set_notes", {
    path: songName,
    track_id: "drum",
    notes: [
      { t: 0.0, pitch: "kick", dur: 0.25, vel: 110 },
      { t: 1.0, pitch: "kick", dur: 0.25, vel: 110 },
      { t: 2.0, pitch: "kick", dur: 0.25, vel: 110 },
      { t: 3.0, pitch: "kick", dur: 0.25, vel: 110 },
    ],
  });
  console.log("[ok] drum set_notes -> 4 kicks");

  // set_instrument (lead を saw_lead に変更)
  const setInst = await callTool("set_instrument", {
    path: songName,
    track_id: "lead",
    type: "saw_lead",
    params: { attack: 0.005 },
  });
  assert(setInst.instrument === "saw_lead", "set_instrument mismatch");
  assert(setInst.previous === "sin", "set_instrument previous mismatch");
  console.log("[ok] set_instrument lead sin -> saw_lead");

  // set_fx (lead に lowpass + reverb)
  const setFx = await callTool("set_fx", {
    path: songName,
    track_id: "lead",
    fx: [
      { type: "lowpass", cutoff: 2000, q: 1.0 },
      { type: "reverb", size: 0.4, mix: 0.2 },
    ],
  });
  assert(setFx.fx_count === 2, `set_fx fx_count != 2 (got ${setFx.fx_count})`);
  console.log("[ok] set_fx -> 2 fx");

  // get_song
  const info = await callTool("get_song", { path: songName });
  assert(info.tracks.length === 2, `get_song tracks != 2 (got ${info.tracks.length})`);
  const leadInfo = info.tracks.find((t) => t.id === "lead");
  assert(leadInfo?.instrument === "saw_lead", "get_song lead instrument mismatch");
  assert(leadInfo?.note_count === 4, "get_song lead note_count mismatch");
  assert(leadInfo?.fx_count === 2, "get_song lead fx_count mismatch");
  console.log("[ok] get_song -> 2 tracks, lead has 4 notes + 2 fx");

  // validate_song
  await callTool("validate_song", { path: songName });
  console.log("[ok] validate_song -> ok");

  // list_songs
  const ls = await callTool("list_songs", {});
  assert(
    ls.songs.some((s) => s.name === "smoke-test"),
    "list_songs missing smoke-test",
  );
  console.log("[ok] list_songs ->", `${ls.songs.length} song(s)`);

  // resource: codetta://songs/{name} — workspace 内の .codetta を生 JSON で読む
  // resources/list で smoke-test が拾われていることも確認
  const res2 = await send("resources/list", {});
  const allResources = (res2.result?.resources ?? []).map((r) => r.uri);
  assert(
    allResources.includes("codetta://songs/smoke-test"),
    `expected codetta://songs/smoke-test in resources/list, got ${JSON.stringify(allResources)}`,
  );
  const songText = await readResource(
    "codetta://songs/smoke-test",
    "application/json",
  );
  const songJson = JSON.parse(songText);
  assert(songJson.version === "0.1", "song resource missing version");
  assert(Array.isArray(songJson.tracks) && songJson.tracks.length === 2, "song resource tracks mismatch");
  const songLead = songJson.tracks.find((t) => t.id === "lead");
  assert(songLead?.instrument?.type === "saw_lead", "song resource lead instrument mismatch");
  assert(Array.isArray(songLead?.notes) && songLead.notes.length === 4, "song resource lead notes mismatch");
  console.log("[ok] resources/read songs/smoke-test -> v0.1, 2 tracks, lead notes intact");

  // resource: 存在しない song はエラー
  {
    const r = await send("resources/read", { uri: "codetta://songs/no-such-song" });
    assert(r.error != null, "expected error for missing song");
    console.log("[ok] resources/read songs/no-such-song -> rejected");
  }

  // set_master_gain (post-mix gain を 2.0 に上げる)
  const setMg = await callTool("set_master_gain", {
    path: songName,
    value: 2.0,
  });
  assert(setMg.master_gain === 2.0, `set_master_gain master_gain != 2.0 (got ${setMg.master_gain})`);
  assert(setMg.previous === 1.0, `set_master_gain previous != 1.0 (got ${setMg.previous})`);
  console.log("[ok] set_master_gain -> 1.0 -> 2.0");

  // render_wav
  const renderStruct = await callTool("render_wav", { path: songName });
  assert(
    typeof renderStruct?.output === "string" && renderStruct.output.endsWith(".wav"),
    "render_wav output is not a .wav path",
  );
  console.log(
    "[ok] render_wav ->",
    renderStruct.output,
    `(${renderStruct.duration_sec}s, ${renderStruct.rtfactor}x rt)`,
  );

  // clear_notes (drum)
  const clr = await callTool("clear_notes", {
    path: songName,
    track_id: "drum",
  });
  assert(clr.removed === 4, `clear_notes removed != 4 (got ${clr.removed})`);
  console.log("[ok] clear_notes drum -> removed 4");

  // remove_track (drum)
  const rmT = await callTool("remove_track", {
    path: songName,
    track_id: "drum",
  });
  assert(rmT.track_id === "drum", "remove_track mismatch");
  const info2 = await callTool("get_song", { path: songName });
  assert(info2.tracks.length === 1, `tracks != 1 after remove_track (got ${info2.tracks.length})`);
  console.log("[ok] remove_track drum -> 1 track remaining");

  console.log("\nAll smoke checks passed (16 tools + 5 resource endpoints).");
  child.kill();
  process.exit(0);
})().catch((e) => {
  console.error("smoke test failed:", e);
  child.kill();
  process.exit(1);
});
