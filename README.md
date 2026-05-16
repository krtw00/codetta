# Codetta

> LLM ネイティブな軽量音楽制作ツール / **The DAW for LLMs**

Codetta は Claude / ChatGPT 等の LLM が **読み・書き・編集** できる音楽プロジェクト形式と、
MCP (Model Context Protocol) 統合により、AI が一級市民として作曲に参加できる
最初の OSS 音楽制作環境を目指します。

## ステータス

🚧 **Phase 1 進行中** — CLI (`codetta build` / `render` / `validate` 等) と MCP server (15 tools + 5 resources) が動作。公開リリースは Phase 4 (`docs/design/00-vision.md` 参照)

## MCP server (Claude Code 等のクライアント向け)

LLM クライアントから Codetta を直接呼び出すための MCP server を同梱しています。

```bash
# CLI と MCP server をビルド
cargo build --release -p codetta-cli
npm install --prefix mcp-server && npm run build --prefix mcp-server

# Claude Code に user scope で登録
claude mcp add --scope user codetta \
  --env CODETTA_BIN=/absolute/path/to/codetta/target/release/codetta \
  --env CODETTA_WORKSPACE=$HOME/codetta-workspace \
  -- node /absolute/path/to/codetta/mcp-server/dist/index.js
```

- `CODETTA_BIN`: CLI 実行ファイルの絶対パス (MCP server から subprocess として呼び出される)
- `CODETTA_WORKSPACE`: LLM が `.codetta` / `.wav` を読み書きする作業ディレクトリ (未存在なら自動作成)

Claude Desktop 用設定、tool / resource 一覧、smoke test の手順等は **[mcp-server/README.md](mcp-server/README.md)** を参照。

## 設計ドキュメント

- [00-vision.md](docs/design/00-vision.md) — ビジョン / スコープ / 競合分析
- [01-architecture.md](docs/design/01-architecture.md) — 全体構成 / コンポーネント / データフロー
- [02-project-format.md](docs/design/02-project-format.md) — プロジェクトファイル形式 (JSON スキーマ)
- [03-cli.md](docs/design/03-cli.md) — CLI コマンド体系
- [04-mcp.md](docs/design/04-mcp.md) — MCP server tools API
- [05-sound.md](docs/design/05-sound.md) — 内蔵音源仕様

## ライセンス

[Apache License 2.0](LICENSE)
