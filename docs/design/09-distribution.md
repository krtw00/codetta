# Codetta — 配布戦略

> Phase 4 マイルストーン。 MIDI import/export (Phase 3) 完走後に着手する。
> 「誰でも `brew install` か GitHub Release からダウンロードして 5 分以内に動かせる」 状態を目指す。
> 本 doc は CDT-11 で起票した設計判断の記録。 最終決定 / 却下済の事項は末尾「決着済」 に整理する。

## 設計原則

1. **初回体験を 1 コマンドで完結させる** — bundle SF2 を同梱し、 インストール直後に `codetta render` が鳴る
2. **配布手段は 2 本立て** — Homebrew tap (Mac / Linux) + GitHub Release バイナリ (Mac / Windows / Linux)
3. **バイナリサイズを抑える** — 本体 < 25 MB、 bundle SF2 込み < 60 MB (= 01-architecture.md 非機能要件)。 CDT-13 前提確認 (2026-05-21) 実測: release binary 1.7MB + SF2 30.8MB ≈ 32.5MB/archive で予算内
4. **ライセンス開示を自動化する** — bundle SF2 (GeneralUser GS) は Apache 2.0 本体と別ライセンス、 インストール時に両方を置く
5. **署名は macOS のみ必須、 他は任意** — macOS は Gatekeeper で弾かれるため notarize 必須。 Windows / Linux は Phase 4 では ad-hoc またはスキップ (= 後述「署名戦略」 参照)

## スコープ (Phase 4)

含める:

- bundle SF2 (GeneralUser GS v2.0.3 相当) の同梱戦略 確定 + 実装
- GitHub Release へのバイナリアップロード (Mac Intel / Apple Silicon / Windows / Linux x64)
- Homebrew tap (`krtw00/codetta`) 作成 + formula 管理
- `LICENSE` / `LICENSE-GeneralUser-GS.txt` の同梱確認
- macOS バイナリの code signing + notarization (= Gatekeeper 対応)
- `README.md` の「インストール方法」「クイックスタート」 整備

含めない (= Phase 5+ で再検討):

- Windows 向け installer (= `.msi` / WiX / winget): Phase 4 では zip 同梱で十分
- Linux パッケージ (= `.deb` / `.rpm` / AUR): Phase 5+ または需要確認後
- macOS universal binary (= Apple Silicon + Intel を 1 本に): 現状は別々アーカイブでよい
- crates.io 公開 (`cargo install codetta`): コア crate の API 安定化後
- GUI 配布 (= `.app` bundle / `electron` 形式): Phase 5+

## bundle SF2 の配布戦略

### 採用案: GitHub Release アーカイブ同梱 + Homebrew formula で DL

SF2 ファイル (= GeneralUser GS v2.0.3、 約 30 MB) をリポジトリに **git-track しない**。 代わりに:

1. **GitHub Release アーカイブ** — `codetta-<version>-<target>.tar.gz` / `.zip` に SF2 を同梱して配布
2. **Homebrew formula** — `resource` ブロックで公式配布 URL から SF2 を DL し、 インストール先 (`prefix/share/codetta/`) に配置
3. **`cargo install` 経由 (= crates.io 公開後)** — インストール後に SF2 DL スクリプト / `codetta setup` サブコマンドで別途取得 (= Phase 4 では未着手)

#### 却下案

| 案 | 却下理由 |
|---|---|
| `include_bytes!` でバイナリ埋め込み | バイナリサイズが +30 MB、 SF2 差し替えが困難。 コンパイル時に SF2 が必要になりリポジトリ管理問題が残る |
| リポジトリに `.sf2` を git-track | LFS なしで 30 MB push は git 履歴を汚す。 LFS 採用は Forgejo / GitHub 両対応で複雑化 |
| git submodule で外部 SF2 リポを参照 | DL 元が外部依存になり URL 変更リスクがある。 Homebrew formula より管理が重い |
| `cargo install` 時に build.rs で自動 DL | ビルド時のネットワーク依存は `cargo install --offline` を壊す。 Rust ポリシーとして推奨されない |

### SF2 の検索パス (= CDT-13 実装確定)

実行時に相対 SF2 file を探す順序 (= `soundfont.rs::resolve_soundfont_path`、 上の速報的 5 段は本実装で置換):

1. **主 path** = `$CODETTA_SOUNDFONT_DIR/<file>` (env 設定時) / 未設定なら `$HOME/Music/sf2/<file>`
2. **bundle dirs** (= `bundle_soundfont_dirs`、 配布物レイアウト): `<bin_dir>` 自身 → `<root>/assets/` → `<prefix>/share/codetta/` → `<bin_dir>/assets/`
3. どれも無ければ主 path を返す (= `SOUNDFONT_FILE_NOT_FOUND` がユーザー向け配置先を指す)

絶対 path はそのまま使う (env / bundle 無関係)。

### bundle SF2 の実行時自動 DL (= CDT-13 実装確定)

**問題**: cargo-dist の curl installer (`installer.sh` / `.ps1`) は **バイナリしか install せず、 `include` した SF2 を破棄する** (= 実機確証)。 tarball 手動解凍では SF2 がバイナリ隣に残るが、 installer ワンライナー経由では SF2 がディスクに存在しない → 「初回体験 1 コマンド」 が成立しない。

**決定**: `codetta render` 実行時、 song が bundle SF2 (= `DEFAULT_SF2` 名) を要求し、 かつ検索パスで見つからない場合に、 専用 data Release (`soundfont-bundle`) から **主 path へ自動 DL** する。 これにより installer 経由でも初回 render で鳴る。

- **トリガ**: render 時のみ (= `validate` は純粋に not-found を報告; core `render_to_buffer` も純粋に保ち、 DL は CLI `render` ハンドラが song 検査して実行 → MCP は CLI spawn で継承)
- **対象**: `DEFAULT_SF2` (= bundle SF2) のみ。 ユーザー指定の任意 SF2 名は DL しない (= URL 不明)
- **配置先**: 主 path (`$CODETTA_SOUNDFONT_DIR` / `~/Music/sf2/`)。 次回以降は主 path で即解決し再 DL しない
- **機構**: `curl` を spawn (= 新規 TLS/HTTP crate を足さず license gate を汚さない、 build-setup.yml と同じ手段)。 DL 後に `sha2` crate で sha256 検証 (= immutable asset、 固定 hash)
- **失敗時**: curl 不在 / network 不通 / hash 不一致 なら `[WARN]` + 手動配置ガイド (= 配置先 + DL URL) を出す。 その後 validate が `SOUNDFONT_FILE_NOT_FOUND` を報告して render は中止する (= ユーザーは案内に従い手動配置)。 自動 DL 失敗自体は panic させない
- **却下**: `include_bytes!` 埋め込み (= ADR 既定で却下、 +30MB×4)、 build.rs DL (= `cargo install --offline` を壊す、 既定で却下)、 explicit `codetta setup` サブコマンド (= 透過自動 DL で UX 上不要)

## GitHub Release バイナリ

### ターゲットトリプル

| OS | アーキテクチャ | ターゲット | アーカイブ形式 |
|---|---|---|---|
| macOS | Apple Silicon | `aarch64-apple-darwin` | `.tar.gz` |
| macOS | Intel | `x86_64-apple-darwin` | `.tar.gz` |
| Windows | x64 | `x86_64-pc-windows-msvc` | `.zip` |
| Linux | x64 | `x86_64-unknown-linux-gnu` | `.tar.gz` |

Windows は `x86_64-pc-windows-msvc` (= MSVC ツールチェーン) を採用。 `x86_64-pc-windows-gnu` は依存 crate (= rustysynth 等) との相性確認が必要なため、 Phase 4 着手時にクロスビルドで検証する。

### アーカイブ構成

```
codetta-cli-aarch64-apple-darwin.tar.gz
└── codetta-cli-aarch64-apple-darwin/
    ├── codetta                            # CLI バイナリ (= archive ルート直下)
    ├── GeneralUser-GS.sf2                 # bundle SF2 (= dist include で archive ルートに平坦化)
    ├── LICENSE                            # Apache 2.0 (codetta 本体)
    ├── LICENSE-GeneralUser-GS.txt         # SF2 のライセンス
    └── README.md
```

> **実装注記 (CDT-13, 2026-05-21 実機確認)**: cargo-dist の `include` はファイルを
> archive **ルート (= バイナリと同階層)** に平坦化する (`bin/` / `assets/` サブディレクトリは作らない)。
> このため bundle SF2 解決は `<bin_dir>` 自身を最優先候補にする (= `soundfont.rs::bundle_soundfont_dirs`)。

MCP server (`mcp-server/`) は GitHub Release には同梱しない。 MCP server は別途 `npm install` / `git clone` + `npm run build` で導入する想定 (= Claude Code の `claude mcp add` と組み合わせる運用)。 Phase 4 では MCP server の配布フローを `README.md` に明示する。

### CI / CD (= GitHub Actions)

`release.yml` workflow:

- `v*.*.*` タグ push をトリガーに起動
- クロスコンパイルは `cross` crate (= Docker ベース) を活用
- macOS バイナリは `macos-latest` (= Apple Silicon runner) でネイティブビルド + code sign + notarize
- Intel Mac バイナリは `macos-13` runner (= Intel) でビルド + sign + notarize
- Windows / Linux は `ubuntu-latest` + `cross` でクロスコンパイル (= Windows は MSVC cross target)
- ビルド完了後に SF2 DL → アーカイブ作成 → `gh release upload` でアタッチ

## Homebrew tap

### tap 名

`homebrew-codetta` リポジトリを `krtw00/homebrew-codetta` として GitHub 公開。
`brew tap krtw00/codetta` + `brew install krtw00/codetta/codetta` でインストール。

### formula 構成 (= `Formula/codetta.rb` 骨子)

```ruby
class Codetta < Formula
  desc "LLM-native piano roll CLI with MCP integration"
  homepage "https://github.com/krtw00/codetta"
  version "0.x.x"

  # macOS Apple Silicon
  on_arm do
    on_macos do
      url "https://github.com/krtw00/codetta/releases/download/v#{version}/codetta-#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "..."
    end
  end

  # macOS Intel
  on_intel do
    on_macos do
      url "https://github.com/krtw00/codetta/releases/download/v#{version}/codetta-#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "..."
    end
  end

  # Linux x64 (= Linuxbrew)
  on_linux do
    url "https://github.com/krtw00/codetta/releases/download/v#{version}/codetta-#{version}-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "..."
  end

  resource "GeneralUser-GS" do
    url "https://dl.codetta.dev/sf2/GeneralUser-GS-v2.0.3.sf2"   # ミラー URL、 確定は Phase 4 で
    sha256 "..."
  end

  def install
    bin.install "bin/codetta"
    (share/"codetta").install resource("GeneralUser-GS")
    doc.install "LICENSE", "LICENSE-GeneralUser-GS.txt"
  end

  test do
    system "#{bin}/codetta", "--version"
  end
end
```

#### SF2 の配布 URL

Homebrew formula の `resource` ブロックに使う SF2 URL は以下の候補から Phase 4 着手時に確定する:

| 候補 | 特徴 | リスク |
|---|---|---|
| `dl.codetta.dev` (自前 CDN / Cloudflare Workers) | URL 管理を自前で持てる。 SHA256 が変わらない | 運用コスト、 Cloudflare Workers の無料枠で賄えるか確認必要 |
| GitHub Release artifact の直リンク | 追加インフラ不要 | GitHub Release URL は sha256 固定で問題なし。 ただし URL が長い |
| SourceForge / 公式サイト直リンク | 追加インフラ不要 | 外部サイト URL 変更リスク。 sha256 検証で破損は検知可能だが URL 自体が消える可能性 |

**決定 (CDT-13、 2026-05-21): 自前再ホスト**。 `GeneralUser-GS.sf2` (v2.0.3、 30.8MB) を自リポの専用 data Release (= tag `soundfont-bundle`) に 1 度だけ upload 済み。 Release archive ビルド (build-setup.yml)・実行時自動 DL・Homebrew `resource` すべてこの immutable URL + 固定 sha256 (`9575028c7a1f589f5770fccc8cff2734566af40cd26ed836944e9a5152688cfe`) を参照する。 tag は**非バージョン形** (= `release.yml` の tag trigger `**[0-9]+.[0-9]+.[0-9]+*` に誤マッチして release workflow を起動しないため)。

理由 / 補足:

- GitHub Release asset は publish 後 immutable なので sha256 が固定でき、 Homebrew formula が upstream 更新で壊れない
- GeneralUser GS License v2.0 は「自分のローカルコピーの再配布」 を許可 (= 公式サイトへの直リンクのみ禁止) → 自前再ホストは license clean
- seed 元: GitHub mirror `mrbumpy409/GeneralUser-GS` repo root の `GeneralUser-GS.sf2` (= v2.0.3、 30.8MB)。 mirror の `main` raw URL は mutable なので、 初回 seed は commit SHA pin で取得して自 Release に上げる
- `dl.codetta.dev` 自前 CDN は不要 (= 需要が出てから再検討)
- 実 upload + tag リリースは CDT-13 本体着手時 (= Apple Dev 反映後の一括実装) に行う

## macOS 署名戦略

### Gatekeeper 対応の必要性

macOS では Apple Developer ID で署名 + notarize しないと Gatekeeper が起動をブロックする (= 「開発元不明」 ダイアログ)。 CLI ツールは「ターミナルで `xattr -dr com.apple.quarantine ./codetta` で回避できる」 とドキュメントに書く手もあるが、 ターゲットユーザー (= 非 DTM、 LLM ユーザー) に quarantine 回避を要求するのは体験として悪い。

→ **macOS バイナリは Apple Developer Program 加入 ($99/年) + Developer ID Installer 証明書で署名 + notarize する**。

### 署名フロー

1. Apple Developer Program 加入 (= 未加入なら Phase 4 開始前に手続き)
2. Developer ID Application 証明書を CI (GitHub Actions の Secrets) に格納
3. `codesign --sign "Developer ID Application: ..."` でバイナリに署名
4. `xcrun notarytool submit ... --wait` で Apple に notarize 申請
5. `xcrun stapler staple` で notarize 結果をバイナリに付加

### Windows / Linux の扱い

- **Windows**: Phase 4 では署名なし。 Windows Defender SmartScreen で警告が出る可能性があるが、「不明な発行元」 クリック回避でインストールできる。 EV コードサイン証明書 (= 約 $300-500/年) は需要確認後に検討
- **Linux**: 署名なし。 AppImage / Flatpak / Snap は Phase 5+ 以降の検討事項

## バージョニングと公開タイミング

### バージョン

Semantic Versioning (`v0.x.y`) を採用。 公開前は `v0.*.*`、 API / schema 安定化後に `v1.0.0`。

`Cargo.toml` の `[workspace.package] version` を唯一の truth とする (= `package.json` 側は MCP server の独立バージョンを持つが、 整合を保つことを CI で確認)。

### 公開タイミングの条件

Phase 4 の GitHub 公開 (= Apache 2.0 OSS として README / Release を外に出す) の条件:

1. `cargo build --workspace` / `cargo clippy` / `cargo fmt` / `cargo test` がすべて green
2. MIDI import/export (Phase 3) が動作確認済み
3. `README.md` の「インストール方法」「クイックスタート」 が完成
4. macOS バイナリの notarize が通っている
5. `LICENSE` と `LICENSE-GeneralUser-GS.txt` が同梱されている

GUI (Phase 5) は公開後も続く作業なので、 Phase 4 公開に含めない。

## ライセンス確認

### codetta 本体: Apache 2.0

`LICENSE` ファイルはすでに存在 (`Cargo.toml` で `license = "Apache-2.0"` 設定済み)。 公開前に年・著者名を確認して更新する。

### bundle SF2: GeneralUser GS License v2.0

GeneralUser GS License v2.0 は **商業利用可、再配布可、改変可** のライセンス (= コピーレフトなし)。 同梱する場合は `LICENSE-GeneralUser-GS.txt` をアーカイブルートに含めること (= 01-architecture.md で既定済み)。

- バンドルするバージョン: GeneralUser GS v2.0.3 (= 2026-05 時点の最新確認版、 Phase 4 着手時に再確認)
- 公式 URL: http://www.schristiancollins.com/generaluser.php

### 依存 crate のライセンス確認

`cargo deny check licenses` で依存ツリー全体のライセンス確認を CI に組み込む (= Phase 4 着手時に `deny.toml` 作成)。

## JSON Schema 公開

`02-project-format.md` では `$schema` フィールドに `https://codetta.dev/schemas/song/0.2` を参照 URL として記載している。 Phase 4 でこの URL を実際にホストする。

### 候補

| ホスト先 | 特徴 |
|---|---|
| GitHub Pages (`krtw00/codetta` の `gh-pages` ブランチ) | 追加インフラ不要、 `https://krtw00.github.io/codetta/schemas/song/0.2` になる |
| Cloudflare Pages / Workers (`codetta.dev` ドメイン) | カスタムドメインで `https://codetta.dev/schemas/song/0.2` になる。 ドメイン取得が前提 |
| GitHub Release artifact 直リンク | URL が長く安定性に難あり (= tag 削除で消える)。 スキーマ URL には不向き |

**暫定採用: Cloudflare Pages + `codetta.dev` ドメイン**。 理由: URL が短くスキーマ URI として安定。 ドメイン取得コスト ($10-15/年) は許容範囲。 GitHub Pages は fallback 候補。

Phase 4 着手時にドメイン取得状況を確認して最終決定する。

## オープンクエスチョン (= Phase 4 着手時に決定)

- [ ] **Apple Developer Program 加入状況** — 加入済 (2026-05、 **反映待ち**)。 Developer ID 証明書が使えるようになり次第 GitHub Secrets に格納し、 CDT-13 で sign/notarize 込みの release workflow を一括実装する (= 反映までは CDT-13 本体を保留)
- [x] **SF2 配布 URL** — **自前再ホスト** に決定 (= 自リポの専用 data Release に SF2 を upload、 上述「SF2 の配布 URL」 参照)
- [ ] **Windows クロスビルドの動作確認** — `cross` + `x86_64-pc-windows-msvc` target でビルドできるか、 `rustysynth` / `hound` との相性確認が必要
- [x] **`codetta setup` サブコマンドの要否** — **不要**。 render 時の透過的自動 DL (= 上述「bundle SF2 の実行時自動 DL」) で代替し、 explicit subcommand は設けない (= MCP tool ペア追加も不要)
- [ ] **MCP server の配布フロー** — `mcp-server/` は Release アーカイブに含めず `npm install` / `git clone` + build 運用とする想定だが、 エンドユーザー向けに `npx` ベース or `@krtw00/codetta-mcp` npm package 公開が現実的か確認
- [ ] **`homebrew-codetta` リポジトリの公開タイミング** — CLI バイナリ公開と同時か、 少し遅らせるか (= 依存: tap formula の SHA256 は Release artifact 確定後に書ける)
- [ ] **JSON Schema ホスト先確定** — `codetta.dev` ドメイン取得 + Cloudflare Pages 設定 (= 暫定採用、 上述「JSON Schema 公開」 参照)

## 決着済

- [x] **bundle SF2 の選定** → **GeneralUser GS v2.0.3** (= 00-vision.md で既定。 初回体験用 GM 互換、 約 30 MB、 商業利用可ライセンス)
- [x] **SF2 をリポジトリに git-track しない** → **却下** (= 上述「却下案」 参照。 GitHub Release + Homebrew resource DL で配布)
- [x] **配布チャネル** → **GitHub Release + Homebrew tap の 2 本立て** (= `cargo install` は API 安定化後)
- [x] **macOS 署名** → **Apple Developer ID で sign + notarize** (= Gatekeeper 対応必須)
- [x] **Windows 署名** → **Phase 4 では署名なし** (= SmartScreen 警告回避はドキュメントでガイド)
- [x] **バージョニング** → **Semantic Versioning `v0.*.*` から開始**
- [x] **MCP server の Release 同梱** → **しない** (= 別途 npm 系で導入する運用)
- [x] **ライセンス** → **本体 Apache 2.0 + bundle SF2 に GeneralUser GS License v2.0 を別添**
- [x] **配布チャネルの実装** → **cargo-dist (`dist`)** で release.yml 生成 (= shell / powershell installer + 4 platform archive)
- [x] **bundle SF2 の installer 経路問題** → **render 時の実行時自動 DL** で解決 (= installer は SF2 を install しないため。 上述「bundle SF2 の実行時自動 DL」 参照)

## 関連ドキュメント

- [00-vision.md](00-vision.md) — Phase 4 の位置付け / bundle SF2 の選定方針 / ライセンス方針
- [01-architecture.md](01-architecture.md) — リポジトリ構成 (`assets/` / `LICENSE-GeneralUser-GS.txt`)、 非機能要件 (バイナリサイズ)
- [03-cli.md](03-cli.md) — `codetta setup` サブコマンド (= Phase 4 で要否判断)
- [04-mcp.md](04-mcp.md) — MCP server 導入フロー
- [07-soundfont.md](07-soundfont.md) — SF2 検索パス / bundle SF2 の扱い
- [08-midi.md](08-midi.md) — import 時の `--sf2` 省略時挙動 (= bundle SF2 フォールバック)
