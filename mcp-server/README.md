# Codetta MCP server

Codetta の [MCP (Model Context Protocol)](https://modelcontextprotocol.io/) server。
`codetta-cli` を subprocess として呼び出す薄い wrapper で、Claude Desktop / Claude Code /
Cursor 等の MCP クライアントから Codetta を直叩きできるようにする。

設計: [`../docs/design/04-mcp.md`](../docs/design/04-mcp.md)

## ステータス

✅ **Phase 1 完走** — `docs/design/04-mcp.md` で定義した全 tool / resource を実装済み。
24bit / 48kHz / トリミング (`from_beat` / `to_beat`) などの Phase 1+ オプションは未対応 (CLI 側も同じ)。

| 区分 | 公開済 | 未対応 |
|---|---|---|
| tools | `create_song` / `get_song` / `validate_song` / `list_songs` / `add_track` / `remove_track` / `set_instrument` / `set_fx` / `set_notes` / `add_notes` / `clear_notes` / `edit_notes` / `set_master_gain` / `render_wav` / `list_instruments` / `list_effects` / `list_soundfont_presets` | — |
| resources | `codetta://songs/{name}` / `codetta://instruments` / `codetta://effects` / `codetta://schema/song/{version}` / `codetta://soundfonts/{name}` / `codetta://presets/{name}` | — |

## セットアップ

### 前提

- Node.js 20+ (推奨 22+)
- `codetta` バイナリ (このリポジトリの `cargo build --release -p codetta-cli` で生成)

### ビルド

```bash
cd mcp-server
npm install
npm run build
```

成果物は `mcp-server/dist/index.js`。

### 環境変数

| 変数 | 既定値 | 説明 |
|---|---|---|
| `CODETTA_BIN` | `codetta` (PATH 解決) | `codetta-cli` の実行ファイル絶対パス |
| `CODETTA_WORKSPACE` | `~/Music/codetta/` | 相対パスの `.codetta` 解決基準。未存在なら自動作成 |
| `CODETTA_PRESETS_DIR` | `<dist>/presets/` (build 時に `<repo>/docs/examples/` をコピー) | プリセット resource の読み込み元 |
| `CODETTA_SOUNDFONT_DIR` | `~/Music/sf2/` | `list_soundfont_presets` / `codetta://soundfonts/{name}` の相対パス解決基準 |

## MCP クライアントへの登録

### Claude Code (CLI)

```bash
claude mcp add --scope user codetta -- node /absolute/path/to/codetta/mcp-server/dist/index.js
```

`CODETTA_BIN` 等の環境変数を渡したい場合は、wrapper シェルスクリプトを噛ませるか、
shell の rc ファイルで export しておく。

### Claude Desktop

`~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) に追記:

```json
{
  "mcpServers": {
    "codetta": {
      "command": "node",
      "args": ["/absolute/path/to/codetta/mcp-server/dist/index.js"],
      "env": {
        "CODETTA_BIN": "/absolute/path/to/codetta/target/release/codetta",
        "CODETTA_WORKSPACE": "/Users/you/Music/codetta"
      }
    }
  }
}
```

## tools 一覧

詳細仕様は [`docs/design/04-mcp.md`](../docs/design/04-mcp.md#tools-一覧) を参照。
ここでは典型的な input 例のみ載せる。

### `list_instruments` / `list_effects` / `list_soundfont_presets`

- `list_instruments` / `list_effects`: 引数なし。内蔵の音源 / エフェクトの一覧とパラメータスキーマを返す。
  `add_track` / `set_fx` / `set_instrument` を呼ぶ前にまずこれで「使える材料」を確認するのが想定フロー。
- `list_soundfont_presets`: `{ "file": "..." }`。SF2 ファイル内の preset 一覧 (bank / preset / name) と SF2 メタ。
  絶対パス、 または `$CODETTA_SOUNDFONT_DIR` (default `~/Music/sf2/`) 配下の相対パス。
  `soundfont` 楽器の `preset` / `bank` を決める前に呼ぶ。

### `create_song`

```jsonc
{
  "path": "battle.codetta",       // 相対なら $CODETTA_WORKSPACE 配下
  "name": "Cyber Battle",         // optional
  "bpm": 140,                     // optional, default 120
  "key": "Am",                    // optional, default "C"
  "time_signature": [4, 4],       // optional, default [4, 4]
  "master_gain": 1.0,             // optional, 0.0-4.0, default 1.0
  "overwrite": false              // optional, default false
}
```

`master_gain` は全 track 合算後 (soft_clip 前) に乗算される post-mix gain。
SF2 系で内蔵合成より peak が低い時のヘッドルーム調整に使う (dogfooding 推奨値 2.0)。
あとから変えたければ `set_master_gain` を呼ぶ。

### `get_song` / `validate_song` / `list_songs`

- `get_song`: `{ "path": "..." }`。 metadata + track 概要 (id / name / instrument / note_count / fx_count) +
  `duration_beats` を返す。ノート詳細は含めない (大きすぎるため) — 必要なら resource
  `codetta://songs/{name}` を読む。
- `validate_song`: `{ "path": "..." }`。スキーマ + 整合性検証。`{ ok: true, valid: bool, errors: [...] }`。
- `list_songs`: 引数なし。 `$CODETTA_WORKSPACE` 配下の `.codetta` を列挙
  (`name` / `path` / `size_bytes` / `modified` ISO8601)。`codetta` バイナリ無しでも動く (Node の `fs` で直接読む)。

### `add_track` / `remove_track`

```jsonc
// add_track
{
  "path": "battle.codetta",
  "track_id": "lead",             // kebab-case 推奨、song 内 unique
  "name": "Lead",                 // optional, default track_id と同じ
  "instrument": "saw_lead",       // optional, default "sin"
  "params": { "filter_cutoff": 1500 }, // optional, 楽器固有
  "volume": 0.8,                  // optional, default 0.8
  "pan": 0.0                      // optional, default 0
}

// remove_track
{ "path": "battle.codetta", "track_id": "lead" }
```

### `set_instrument` / `set_fx`

```jsonc
// set_instrument: 楽器 type + params を完全置換
{
  "path": "battle.codetta",
  "track_id": "lead",
  "type": "saw_lead",
  "params": { "attack": 0.001, "filter_cutoff": 2500 }
}

// set_fx: fx チェーン全置換 (空配列でクリア)
{
  "path": "battle.codetta",
  "track_id": "lead",
  "fx": [
    { "type": "delay", "time": "1/8", "feedback": 0.3, "mix": 0.25 },
    { "type": "reverb", "size": 0.5, "mix": 0.2 }
  ]
}
```

### `set_notes` / `add_notes` / `clear_notes` / `edit_notes`

```jsonc
// set_notes: 全置換 (冪等)
{
  "path": "battle.codetta",
  "track_id": "lead",
  "notes": [
    { "t": 0.0, "pitch": "A4", "dur": 0.5, "vel": 100 },
    { "t": 0.5, "pitch": "C5", "dur": 0.5, "vel": 100 }
  ]
}

// add_notes: 追加 (完全一致は skipped_duplicates に計上)
{ "path": "battle.codetta", "track_id": "lead", "notes": [ /* 同上 */ ] }

// clear_notes: 全削除
{ "path": "battle.codetta", "track_id": "lead" }

// edit_notes: 一括変形 (順次適用)
{
  "path": "battle.codetta",
  "track_id": "lead",
  "ops": [
    { "op": "transpose", "semitones": -12 },
    { "op": "quantize", "grid": 0.25 }
  ]
}
```

`pitch` は `"A4"` 等の文字列 / MIDI 番号 / `drum_kit` のキー (`"kick"` 等)。

### `set_master_gain`

```jsonc
{
  "path": "battle.codetta",
  "value": 2.0                    // 0.0-4.0
}
```

`metadata.master_gain` を変更する。全 track 合算後 (soft_clip 前) に乗算される post-mix gain。

### `render_wav`

```jsonc
{
  "path": "battle.codetta",
  "output": "out.wav",            // optional, default <path>.wav
  "sample_rate": 44100            // optional, 44100 | 48000
}
```

レスポンスは `{ ok, output, duration_sec, render_time_sec, rtfactor, ... }`。
WAV のバイト列は**返さない** (大きすぎる) — LLM はパスを返すまでで、再生はユーザーが行う。

## resources 一覧

| URI | 内容 |
|---|---|
| `codetta://songs/{name}` | `$CODETTA_WORKSPACE/{name}.codetta` の生 JSON |
| `codetta://instruments` | 内蔵楽器のカタログ (`list_instruments` tool と同内容) |
| `codetta://effects` | 内蔵エフェクトのカタログ (`list_effects` tool と同内容) |
| `codetta://schema/song/{version}` | プロジェクトファイル JSON Schema (`version` は `schema_version` と一致必須) |
| `codetta://soundfonts/{name}` | `$CODETTA_SOUNDFONT_DIR/{name}.sf2` の preset 一覧 + メタ (`list_soundfont_presets` tool と同内容) |
| `codetta://presets/{name}` | `docs/examples/{name}.codetta` のサンプル曲 (read-only) |

`{name}` は拡張子を除いたファイル名 (例: `cyber-lead`, `GeneralUser-GS-v1.471`)。
クライアントが `resources/list` を呼ぶと、 各 template が workspace / SF2 dir / presets dir をスキャンして
動的に列挙される。

## 開発

### 型チェック / ビルド

```bash
npm run typecheck   # tsc --noEmit
npm run build       # tsc -> dist/
npm run watch       # tsc --watch
```

### Smoke test

`scripts/smoke.mjs` が initialize → tools/list → resources/list → tool 呼び出し → resource read を
end-to-end でテストする (build 後に実行):

```bash
cargo build --release -p codetta-cli   # CODETTA_BIN 用バイナリを用意
npm run build --prefix mcp-server
node mcp-server/scripts/smoke.mjs
```

## ライセンス

[Apache License 2.0](../LICENSE) (リポジトリ全体)
