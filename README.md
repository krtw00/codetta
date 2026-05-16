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

## SoundFont (.sf2) を使う

内蔵音源 (`sin` / `saw` / `square` / `triangle` / `saw_pad` / `drum_kit`) では届かないピアノ / 弦 / ブラス等の生楽器音色は、外部 SoundFont (`.sf2`) を持ち込んで補えます。Codetta 本体には SF2 を同梱しないため、ユーザー自身で OSS SF2 を取得して配置してください。

### 1. SF2 ファイルを取得

| 名称 | 配布元 | サイズ | ライセンス |
|---|---|---|---|
| **GeneralUser GS** (推奨、235 preset の高品質 GM/GS) | <https://www.schristiancollins.com/generaluser.php> | ~30MB | free for any use |
| **TimGM6mb** (軽量、PoC / テスト向け) | <https://github.com/arbruijn/TimGM6mb> | ~6MB | GPL-2 |
| **FluidR3_GM** (MuseScore でも使われるクラシック標準) | <https://github.com/Jacalz/fluid-soundfont> | ~140MB | MIT |

各 SF2 のライセンス条件は配布元で確認してください。

### 2. 配置先 (`$CODETTA_SOUNDFONT_DIR`)

相対パスで指定された SF2 ファイルは `$CODETTA_SOUNDFONT_DIR` 配下から解決されます。未設定の場合は `$HOME/Music/sf2/` がデフォルト。

```bash
mkdir -p ~/Music/sf2
# 例: GeneralUser-GS-v1.471.sf2 を ~/Music/sf2/ に配置

# 別の場所に置きたい場合は env で上書き
export CODETTA_SOUNDFONT_DIR="$HOME/path/to/soundfonts"
```

絶対パスを直接指定することもできます。

### 3. SF2 トラックを含む song を作る

```bash
codetta new sf2-demo.codetta --bpm 100 --force
codetta add-track sf2-demo.codetta --id piano --name Piano \
  --instrument soundfont \
  --params-json '{"file":"GeneralUser-GS-v1.471.sf2","preset":0}'
echo '[{"t":0,"pitch":"C4","dur":1},{"t":1,"pitch":"E4","dur":1},{"t":2,"pitch":"G4","dur":1}]' > notes.json
codetta set-notes sf2-demo.codetta --track piano --notes-file notes.json
codetta validate sf2-demo.codetta   # SOUNDFONT_FILE_NOT_FOUND が出なければ OK
codetta render sf2-demo.codetta --output sf2-demo.wav
```

`params.preset` は GM Program 番号 (0 = Acoustic Grand Piano)、`params.bank` は省略時 0。SF2 ファイルが見つからない場合は `validate` が `SOUNDFONT_FILE_NOT_FOUND` を報告します。

詳しい仕様 / render path / 制約は **[docs/design/07-soundfont.md](docs/design/07-soundfont.md)** を参照。

## 設計ドキュメント

- [00-vision.md](docs/design/00-vision.md) — ビジョン / スコープ / 競合分析
- [01-architecture.md](docs/design/01-architecture.md) — 全体構成 / コンポーネント / データフロー
- [02-project-format.md](docs/design/02-project-format.md) — プロジェクトファイル形式 (JSON スキーマ)
- [03-cli.md](docs/design/03-cli.md) — CLI コマンド体系
- [04-mcp.md](docs/design/04-mcp.md) — MCP server tools API
- [05-sound.md](docs/design/05-sound.md) — 内蔵音源仕様
- [07-soundfont.md](docs/design/07-soundfont.md) — SoundFont (SF2) optional 拡張

## ライセンス

[Apache License 2.0](LICENSE)
