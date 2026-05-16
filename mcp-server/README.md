# Codetta MCP server

Codetta の [MCP (Model Context Protocol)](https://modelcontextprotocol.io/) server。
`codetta-cli` を subprocess として呼び出す薄い wrapper で、Claude Desktop / Claude Code /
Cursor 等の MCP クライアントから Codetta を直叩きできるようにする。

設計: [`../docs/design/04-mcp.md`](../docs/design/04-mcp.md)

## ステータス

🚧 **Phase 1 スケルトン** — Phase 0 (CLI) が完走したのを受けて最小機能を expose した状態。
Phase 1 で残りの tool (`add_track` / `set_instrument` / `set_fx` / `edit_notes` / `get_song` 等) を順次追加予定。

| 区分 | 公開済 | 未実装 |
|---|---|---|
| tools | `list_instruments` / `list_effects` / `create_song` / `set_notes` / `render_wav` | `get_song` / `validate_song` / `list_songs` / `add_track` / `remove_track` / `set_instrument` / `set_fx` / `add_notes` / `clear_notes` / `edit_notes` |
| resources | `codetta://presets/{name}` (docs/examples/) | `codetta://songs/` / `codetta://instruments` / `codetta://effects` / `codetta://schema/song/{version}` |

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
| `CODETTA_WORKSPACE` | `~/codetta-songs/` | 相対パスの `.codetta` 解決基準。未存在なら自動作成 |
| `CODETTA_PRESETS_DIR` | `<repo>/docs/examples/` | プリセット resource の読み込み元 |

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
        "CODETTA_WORKSPACE": "/Users/you/codetta-songs"
      }
    }
  }
}
```

## tools 一覧

### `list_instruments` / `list_effects`

引数なし。Codetta 内蔵の音源 / エフェクトの一覧とパラメータスキーマを返す。
`add_track` / `set_fx` を呼ぶ前にまずこれで「使える材料」を確認するのが想定フロー。

### `create_song`

```jsonc
{
  "path": "battle.codetta",       // 相対なら $CODETTA_WORKSPACE 配下
  "name": "Cyber Battle",         // optional
  "bpm": 140,                     // optional, default 120
  "key": "Am",                    // optional, default "C"
  "time_signature": [4, 4],       // optional, default [4, 4]
  "overwrite": false              // optional, default false
}
```

### `set_notes`

```jsonc
{
  "path": "battle.codetta",
  "track_id": "lead",
  "notes": [
    { "t": 0.0, "pitch": "A4", "dur": 0.5, "vel": 100 },
    { "t": 0.5, "pitch": "C5", "dur": 0.5, "vel": 100 }
  ]
}
```

ノート列を**全置換**する (冪等)。`pitch` は `"A4"` 等の文字列 / MIDI 番号 / drum_kit のキー (`"kick"` 等)。

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
| `codetta://presets/{name}` | `docs/examples/{name}.codetta` の中身 (application/json) |

`{name}` は拡張子を除いたファイル名 (例: `cyber-lead`, `cyber-battle-full`)。
クライアントが `resources/list` を呼ぶと、`docs/examples/` 配下の `.codetta` が動的に列挙される。

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
