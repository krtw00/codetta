# Codetta — ビジョン / スコープ

> **Codetta** = `code` + `coda` (音楽用語: 楽曲の終結部)
> LLM ネイティブな piano roll CLI。 note データを LLM が読み書き編集できる JSON 形式 (`.codetta`) と
> MCP 統合により、 Claude や ChatGPT が「作曲する側」 として一級市民で参加できることを最大の差別化とする。
> 音源は外付け SoundFont (SF2) に統一、 外部連携は MIDI 等の一般フォーマットで行う。

## ビジョン

LLM が **読み・書き・編集できる** 音楽制作環境を作る。

既存 DAW (Cubase / Logic / FL 等) はバイナリ形式のプロジェクトファイル + マウス操作前提の UI を持つため、
LLM が「曲を作る」 「部分修正する」 「フィードバックに応じてリビジョンする」 ことが構造的に困難。
Codetta はこの制約を取り払い、 **LLM が一級市民として作曲に参加できる最初の音楽ツール** を目指す。

音源は自前で持たず、 外付け SoundFont (SF2) を呼び出して鳴らす。 これにより:

- codetta コードベースは「note データの編集 + 外付け音源への dispatch + 出力 (WAV / MIDI)」 に集中
- 音色の品質は SF2 の品質に従う (= 既存の高品質フリー SF2 を活用、 自前 voicing 改善は方向性外)
- 外部 DAW / 外部 player との往復は MIDI 形式で完結

## ターゲットユーザー

優先順位順:

1. **個人ゲーム開発者 / インディー作家** — BGM / SE が必要だが DTM スキルを習得する余裕がない層
2. **LLM で作曲を試みる開発者** — Claude Code / Cursor / Claude Desktop ユーザー、 LLM ペアプロを音楽制作にも持ち込みたい層
3. **ゲーム素材として SF2 で生楽器寄せの BGM / SE を作りたい層** — DAW を起動するほどではない、 軽い素材作りニーズ

明示的に **ターゲット外**:

- プロの DTM ユーザー (Cubase / Pro Tools の置き換えではない)
- 録音中心のミュージシャン
- VST / AU プラグインを大量利用するワークフロー前提のユーザー
- チップチューン愛好家 (= 内蔵 synth を持たない方針なので、 ピコピコ電子音特化のニーズには応えない)

## 差別化 (3 本柱)

### 1. LLM ネイティブ (最大の差別化)

- MCP server を **コア提供物** として位置付ける (添え物ではない)
- プロジェクトファイル = 人間も LLM も読める JSON
- Claude が tools 経由で `create_song` / `add_notes` / `render_wav` / `import_midi` / `export_midi` を呼べる
- 「ダーク感をもう少し」 のようなフィードバックで部分修正可能

競合状況: 既存 DAW で MCP 対応はほぼ無し (2026-05 時点)。 **空き地ポジション**。

### 2. テキスト中心 (Git フレンドリー)

- プロジェクトファイル `.codetta` は JSON (バイナリブロブなし)
- `git diff` / `git merge` が意味を持つ
- 「曲のスニペット」 をテキストで共有可能
- LLM プロンプトに直接コピペできる

### 3. 外部連携 (connectivity)

- **外付け SF2 (SoundFont) で鳴らす** — bundle 配布の SF2 で初回体験完結、 ユーザーは好きな SF2 に差し替え可能
- **MIDI で外部 DAW / 外部 player と往復** — 「codetta で叩き作って、 DAW で続きをやる」 のような自然な行き来
- 「内蔵だけで完結」 ではなく、 「外と気持ちよくつながる」 が codetta の立ち位置

## スコープ

### 含める

| 機能 | 状態 |
|---|---|
| MIDI 打ち込み (内部 JSON 表現、 `.codetta`) | ✅ Phase 0-1 完走 |
| 外付け SoundFont (SF2) 再生 | ✅ Phase 1+ 完走 |
| 簡易エフェクト (Reverb / Delay / Filter / Distortion) | ✅ Phase 0 完走 |
| WAV エクスポート (オフラインレンダリング、 SF2 経由) | ✅ Phase 0 完走 |
| Master gain (post-mix 音量調整) | ✅ Phase 1+ 完走 |
| MCP server (Claude 連携) | ✅ Phase 1 完走 |
| **内蔵 synth 削除 + schema 0.2 化** | ✅ 完走 (CDT-7、 SF2 一本化) |
| **MIDI import / export** | 次マイルストーン |
| **配布整備** (bundle SF2 / homebrew tap / binary release) | 次マイルストーン |
| **GUI (piano roll、 再生、 微調整)** | 必須マイルストーン (= 「人間が触るなら GUI ないと話にならん」) |

### 含めない (明示的に却下)

| 機能 | 却下理由 |
|---|---|
| **内蔵 synth (自前 DSP voicing)** | 方向性外。 音色は外付け SF2 に任せる (詳細は ADR-001 相当の設計判断、 0.2 化で削除) |
| **VST3 / AU プラグインホスト** | C++ 必須で開発期間 +1 年、 配布敷居が上がる (= 「そこまでするなら普通の DAW」)。 将来再検討の余地は残す |
| **オーディオ録音** | 別問題、 スコープ膨張要因 |
| **マスタリング機能** | プロ用途、 ターゲット外 |
| **プラグイン SDK 提供** | 外部開発者向けは将来検討 |
| **スコア (五線譜) UI** | 学習コストが高く、 ターゲットユーザーに不要 |
| **大規模サンプルライブラリの自前ホスト** | 配布サイズ膨張。 bundle するのは「初回体験用の 1 つの GM 互換 SF2」 のみ (現状 GeneralUser GS v2.0.3 約 30MB を予定) |
| **クラウド同期 / 共同編集** | サーバ運用コスト、 将来の課金導線候補 |

## 競合と差別化マップ

| 製品 | 軽量 | LLM 連携 | テキスト形式 | 外部連携 | OSS | 価格 |
|---|---|---|---|---|---|---|
| Cubase / Pro Tools | ✗ | ✗ | ✗ | △ | ✗ | 高額 |
| Logic Pro | ✗ | ✗ | ✗ | △ | ✗ | 中 |
| REAPER | ◎ | ✗ | ✗ | ○ | ✗ | $60 |
| LMMS | ○ | ✗ | △ | ○ | ◎ GPL | 無料 |
| GarageBand | ○ | ✗ | ✗ | △ | ✗ | 無料 (Mac) |
| BeepBox | ◎ | ✗ | ○ | △ | ◎ | 無料 |
| BoscaCeoil | ◎ | ✗ | ✗ | △ | ◎ | 無料 |
| Domino | ◎ | ✗ | △ | ○ MIDI | ✗ フリーソフト | 無料 |
| Suno / Udio | — | △ 生成のみ | ✗ | ✗ | ✗ | 月額 |
| **Codetta** | **○** | **◎** | **◎** | **◎ SF2 + MIDI** | **◎ Apache 2.0** | **無料** |

**Codetta は LLM 連携 × テキスト形式 × 外部連携 × OSS の組み合わせで唯一の存在** になる。
軽量さは「○」 に後退 (= bundle SF2 約 30MB を含むため超軽量とは言わない)。

## ライセンス方針

- **本体: Apache 2.0** (codetta-core / codetta-cli / mcp-server / codetta-gui)
- **bundle SF2: GeneralUser GS License v2.0** (= 同梱 `LICENSE-GeneralUser-GS.txt` で別個に開示、 codetta 本体のライセンスとは独立)

理由 (本体 Apache 2.0):

- MIT 並みの自由さ
- 特許条項 (LLM 連携で将来トラブル余地)
- 大手 OSS (Kubernetes / Spark / Android) と同じで信頼感あり
- 将来の Open Core (Pro 機能を別ライセンス) 余地を残せる

理由 (bundle SF2 を別ライセンス分離):

- SF2 のライセンス (GeneralUser GS License v2.0) はコピーレフトではないが、 本体と異なる原典なので独立して開示
- 「自分の好きな SF2 を bundle 差し替えしたい」 配布派生にも対応しやすい

将来の課金導線 (公開後に判断):

- 案 A: 純 OSS のまま、 寄付のみ (GitHub Sponsors)
- 案 B: コアは無料、 Pro 機能 (クラウド同期 / プレミアム SF2 / 共同編集) を別途有料
- 案 C: ホスト型 SaaS (Web 版 + クラウド) を有料、 ローカル版は無料

現時点では決定しない。 **初回公開時点は純粋 OSS として運用**。

## ネーミングの由来

- **Codetta** = `code` + `coda` (音楽用語: 楽曲の終結部、 添え書き)
- 「コード (プログラム) で曲を書く」 「LLM が最後の仕上げ (coda) を担う」 のダブルミーニング
- 商標衝突調査: 既存ソフトウェア名と被りなし (2026-05 時点、 要再確認)

## 段階リリース計画

完走済 phase (= 既に動いている部分):

| Phase | 内容 | 状態 |
|---|---|---|
| 0 | Rust CLI コア (JSON → WAV、 内蔵 synth + SF2 並列) | ✅ |
| 1 | MCP server (TS) | ✅ |
| 1+ | SoundFont (SF2) 統合 + master_gain | ✅ |
| 2 | 内蔵 synth 削除 + schema 0.2 化 (SF2 一本化、 migrate / drum 要素名キー 正規化) | ✅ 完走 (CDT-5/6/7/8) |

次以降のマイルストーン (順序は依存関係で固定):

| Phase | 内容 | 期間目安 |
|---|---|---|
| **3** | MIDI import / export | 2-3 週間 |
| **4** | 配布整備 (bundle SF2 / homebrew tap / GitHub Release / Apache 2.0 公開) | 1-2 週間 |
| **5** | **GUI (piano roll、 再生、 微調整)** — 必須マイルストーン | 4-6 週間 |
| 6+ | 公開後の機能拡張 / 課金導線検討 | 継続 |

GUI フレームワーク選定 (egui / Tauri / Dioxus 等) は Phase 5 開始時に最終決定。
それまでに CLI で piano roll 操作の使い勝手を詰める方針 (= 「まず CLI で作り込んで、 そこから GUI に落とし込む」)。

## 成功指標 (公開後)

| 指標 | 6 ヶ月後目標 |
|---|---|
| GitHub Stars | 100+ |
| アクティブユーザー (DL 数) | 500+ |
| MCP 経由生成曲数 (テレメトリ取らないので推定) | — |
| 自分の dogfood (= 個人ゲーム開発で採用した曲数) | 10+ |

dogfood は **自分が primary target user** (= ゲーム開発者 + LLM 作曲試行者 + SF2 で素材作りたい層) でもあるため、
「自分が触って気持ち良いか」 を 1 次評価とする。
