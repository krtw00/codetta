# Codetta — MIDI import/export

> 外部 DAW / Web 共有 MIDI と `.codetta` の往復経路。 Phase 3 マイルストーン。
> `.codetta` の音源は SF2 一本 (= schema 0.2) なので、 MIDI 側は GM (General MIDI) 互換を基盤とし、
> codetta 固有の拡張属性 (`master_gain` / fx チェーン / SF2 preset 詳細) は **MIDI Text Meta Event に JSON inline** で
> 埋め込むことで往復可能にする。
> 本 doc は CDT-2 (MIDI import/export ADR) で決着した設計判断の確定版。 実装着手は CDT-3 (import) / CDT-4 (export)。

## 設計原則

1. **GM 互換を基盤** — 外部 DAW / 一般 MIDI player が `.mid` だけで「とりあえず鳴る」 状態を維持
2. **拡張属性は MIDI 内に閉じる** — sidecar JSON は optional 退避経路に格下げ、 default は単一 `.mid` ファイル完結
3. **round-trip 一貫性** — `.codetta → .mid → .codetta` で意味的同値 (= 同じ JSON shape) を保つ
4. **schema 0.2 専用** — `.codetta` 側は SF2 一本前提。 0.1 legacy ファイルは export 前に implicit migrate を通す
5. **失敗を握り潰さない** — マッピング失敗は warning + best-effort fallback + 明示。 silent drop は避ける

## スコープ (Phase 3)

含める:

- 基本 MIDI Type 1 (multi-track) の読み書き
- channel ↔ track の 1:1 マッピング (= A 案)
- GM Program (0-127) ↔ SF2 preset の双方向対応 (= drum channel 10 含む)
- ドラム要素名キー (`"kick"` / `"snare"` 等) の MIDI note number 展開
- 拡張属性 round-trip (`master_gain` / fx チェーン / SF2 preset 詳細)
- tempo / time signature の往復
- `--extensions` モード切替 (`text-meta` / `sidecar` / `none`)
- CLI `export-midi` / `import-midi` + MCP `export_midi` / `import_midi`

含めない (= Phase 5+ で再検討):

- MIDI Type 0 (single-track, 全 channel mix) の生成 (import 読込のみ対応)
- SysEx / CC オートメーション (= 02-project-format.md「拡張ポイント」 と同期、 `cc_events[]` 導入時に追記)
- パターン (`patterns[]`) の MIDI 表現 (= 02 で Phase 5+ 拡張)
- マーカー / 複数 tempo / 拍子変化 (= 02 で Phase 5+ 拡張)
- MIDI 2.0 (= 採用しない、 SMF 1.0 ベース固定)

## ファイル形式

| 項目 | 採用値 | 補足 |
|---|---|---|
| MIDI Type | **Type 1** (multi-track) | export は常に Type 1。 import は Type 0 / Type 1 両対応 |
| PPQ (ticks per quarter) | **480** (default、 `--ppq` で上書き可) | DAW で一般的、 32 分音符 3 連 (= 1/96 拍) まで誤差なし表現可 |
| tempo | `metadata.bpm` を **`MTrk` 0 先頭 1 つ**で `FF 51 03` (= microseconds per quarter) として書く | tempo 変化は Phase 5+ で再考、 Phase 3 は曲全体固定 |
| time signature | `metadata.time_signature` を **`MTrk` 0 先頭 1 つ**で `FF 58 04` として書く | 拍子変化は Phase 5+ |
| velocity | `notes[].vel` (0-127) をそのまま MIDI velocity に | 省略時 codetta default `100` を書く (= 02-project-format.md 規約) |
| note off velocity | 固定 64 (= `0x40`) | release velocity を持たない codetta スキーマと整合 |
| 時間表現 | beat (codetta) ↔ tick (MIDI) は `tick = round(beat * ppq)` | float beat の量子化誤差は `ppq=480` で 1/480 拍まで、 通常用途は無視可 |
| 文字エンコーディング | Text Meta Event は **UTF-8 として書く** | SMF 仕様は 7-bit ASCII を「望む」 とするが、 codetta 拡張属性は ASCII 範囲 (JSON) 限定で衝突なし |

依存 crate: `midly`、 採用は 01-architecture.md で確定済。

## channel ↔ track マッピング

### 採用案: A 案 (1 track = 1 channel、 順序固定)

`tracks[]` の出現順に MIDI channel を割り当てる。 drum track (= `instrument.params.bank == 128`) だけは channel **10** (= MIDI ch index 9、 0-origin) に予約。

```
tracks[]              MIDI channel (1-origin / 0-origin index)
─────────             ─────────────────────────────────────
tracks[0] melodic →   ch1  (idx 0)
tracks[1] melodic →   ch2  (idx 1)
tracks[2] drum   →    ch10 (idx 9)       ← bank 128 は ch10 固定
tracks[3] melodic →   ch3  (idx 2)
tracks[4] melodic →   ch4  (idx 3)
```

melodic 用には ch1-9 + ch11-16 = **15 channel** が使える。 drum は ch10 1 本固定。

### export: track → channel

1. `tracks[]` を順に走査
2. drum track (= `bank == 128`) は ch10 へ送る。 複数 drum track があれば **`MULTIPLE_DRUM_TRACKS_NOT_SUPPORTED` エラー**で abort (= silent merge しない)
3. melodic track には ch1, ch2, ..., ch9, ch11, ch12, ..., ch16 を順に割り当てる
4. 16 channel を超える melodic track があれば **`TRACK_LIMIT_EXCEEDED` エラー** (= 15 melodic + 1 drum)
   - 救済策の自動実行はしない (= 「自動で merge / 自動で別ファイル分割」 はせず、 ユーザーに `set_instrument` で統合 or 手動分割を促す)
   - ただしエラーレスポンスには「超過した track id 一覧」 を含める
5. 各 track の `MTrk` 先頭で Program Change (`Cn pp`) を書く (= GM Program 番号)、 続けて Control Change で volume (CC7) / pan (CC10) を書く

### import: channel → track

1. MIDI を走査して使用 channel の集合を作る
2. ch10 (= idx 9) があれば → `bank: 128, preset: 0` の drum track として 1 つ追加
3. それ以外の channel (ch1-9, 11-16) を順に track として展開、 順序は **channel index 昇順** (= 自然に ch1 → ch2 → ... → ch10 (drum) → ch11 → ... と並ぶ)。 これは text-meta の `tracks[]` 並び (= L190) と合わせるための実装規約 (= CDT-3 で確定)
4. 各 track の `instrument.params.preset` は channel の Program Change の最終値 (= 同 channel 内で複数 Program Change がある MIDI は最後の値が勝つ)、 Program Change が一度も無い channel は `preset: 0`
5. CC7 / CC10 を `volume` / `pan` に変換 (= 0-127 を 0.0-1.0 と -1.0-1.0 に正規化)

### 17 track 以上を超過した場合の理由付け

A 案を採用したのは「DAW import 時の column 表示が 1:1 で直感的」 / 「LLM が読み書きする時に channel と track が混在しなくて済む」 ため。

却下案:

- **B 案 (program ベース集約)**: 「同じ GM Program を持つ複数 track を 1 channel に集約、 track 区別は Track Name Meta Event で復元」 → import 時に元の track 構成が確定できない (= note 衝突時に元 track へ戻せない)。 却下
- **C 案 (自動 split)**: 「17 track 以上の場合 `.mid` を複数本に自動分割」 → 単一 `.mid` の利点を捨てる、 LLM が DAW にもらう MIDI が複数本になる想定は薄い。 却下

将来 (Phase 5+) で 16 channel 超過が現実的問題になれば「MIDI Track (= `MTrk`) を増やして同 channel に異 track 分の note を流す」 等を再検討。 Phase 3 では超過 = エラーで明示する。

## drum channel (MIDI 10) の扱い

### export

- `tracks[]` の中で `instrument.params.bank == 128` を持つ track が drum track
- channel は ch10 (= idx 9) 固定
- ノートの `pitch` 処理:
  - 要素名キー (`"kick"` / `"snare"` / `"hh_closed"` 等) → GM Drum Map の MIDI 番号に展開 (= 07-soundfont.md L161 の対応表)
  - ノート名 (`"C4"` 等) → 通常通り MIDI 番号に変換
  - MIDI 番号 (int) → そのまま
- Program Change は **書かない** (= ch10 は GM の暗黙約束で「drum kit」、 GM Program 番号は無視される)
- ただし非標準 drum kit を使う場合 (= `preset != 0`) は Program Change を書く (= GeneralUser GS では preset 0=Standard / 8=Room / 16=Power / 24=Electronic 等、 受け側 SF2 が対応していれば差替えできる)

### import

- ch10 (= idx 9) のチャンネルを **`bank: 128, preset: 0` の drum track** として 1 つ作る
  - Program Change がある場合はその値を `preset` に採用 (= 非標準 drum kit MIDI 互換)
- 各ノートの note number は **数値 MIDI 番号 (= int) のまま `pitch` に書く**
- **逆変換 (= MIDI 番号 → 要素名キー) は default では行わない**

数値固定の理由:

`.codetta` の note を `pitch: "kick"` で書いていたユーザーが export → import すると `pitch: 36` に変わってしまうが、 これは意図通り (= round-trip の意味的同値は保たれている、 JSON shape は変わるが描画 / 再生は同一)。

逆方向 (= MIDI 番号 → 要素名キー) を default にすると問題が起きる:

- `pitch: 37` (= Side Stick) のような GM Drum Map に「要素名キーが無い」 ノートだけ数値で残り、 同じ track 内で要素名と数値が混在する一貫性の崩れた JSON ができる
- LLM が後から `set_notes` で書き戻す時、 元の表記 (= 要素名 or 数値) を覚えていないので diff が荒れる

→ 「LLM フレンドリーな要素名キーは作曲時の入口専用、 round-trip では数値で保存」 という割り切りを採る。

### 例外オプション (任意): `--humanize-drum-keys`

`import-midi --humanize-drum-keys` で逆変換を有効化可。 default = off。 LLM が import 直後に編集を続けたい時の救済。 MCP では `import_midi(humanize_drum_keys=true)` で同等。 Phase 3 着手時に実装優先度を判断、 初回リリースでは見送り可。

## schema 拡張属性の round-trip

### 採用案: `text-meta` (= MIDI Text Meta Event に JSON inline)

`--extensions text-meta` (default) では、 codetta 拡張属性 (`master_gain` / track ごとの fx チェーン / SF2 preset 詳細) を **1 つの Text Meta Event** に **JSON inline** で埋め込む。

#### 配置と形式

- `MTrk` 0 (= 最初の MIDI Track、 tempo / time sig を持つ「メタトラック」) の **先頭 (delta=0)** に書く
- イベントタイプ: `FF 01 <var-length-len> <utf8 bytes>` (= Text Meta Event)
- payload は ASCII 範囲の JSON 1 オブジェクト、 改行なし (= 単一行)

##### payload スキーマ

```json
{
  "codetta": {
    "version": "0.2",
    "metadata": {
      "master_gain": 2.0,
      "key": "Am",
      "tags": ["ddc", "battle", "cyber"]
    },
    "tracks": [
      {
        "id": "lead",
        "name": "Saw Lead",
        "instrument": {
          "type": "soundfont",
          "params": { "file": "GeneralUser-GS-v1.471.sf2", "preset": 81, "bank": 0 }
        },
        "fx": [
          { "type": "delay", "time": "1/8", "feedback": 0.3, "mix": 0.25 },
          { "type": "reverb", "size": 0.5, "mix": 0.2 }
        ]
      },
      {
        "id": "drums",
        "name": "Drums",
        "instrument": {
          "type": "soundfont",
          "params": { "file": "GeneralUser-GS-v1.471.sf2", "preset": 0, "bank": 128 }
        },
        "fx": []
      }
    ]
  }
}
```

含めるもの:

- `version` (= schema 0.2 固定、 import 時の検証用)
- `metadata.master_gain` / `metadata.key` / `metadata.tags` (= MIDI 標準で表現できない属性)
- `tracks[]` の `id` / `name` / `instrument` / `fx` (= MIDI Program Change だけでは復元できない SF2 preset 詳細 + fx チェーン)
- `tracks[]` の順序は MIDI channel 順 (= ch1 → ch2 → ... → ch10 (drum) → ch11 → ...)

含めないもの:

- `notes[]` (= MIDI event 本体で表現するので二重持ちしない)
- `volume` / `pan` (= CC7 / CC10 で MIDI 側に書く、 import 時はそちらから復元)
- `mute` / `solo` (= ミックス状態は MIDI に出さない、 round-trip では `false` / `false` で初期化)
- `metadata.bpm` / `metadata.time_signature` (= `FF 51` / `FF 58` で MIDI 側に書く、 import 時はそちらから復元)

### 採用理由

- 単一ファイル完結 (= sidecar に分離するとファイル共有時に拡張属性が落ちるリスク)
- JSON は codetta 既存 schema との一貫性が高い (= 02-project-format.md の JSON shape そのまま)
- Text Meta Event の **length 制限は実質なし** (= MIDI 仕様の var-length 表現で 256MB 超まで可、 拡張属性 JSON は典型 1-10 KB に収まる)
- import 時に Text Meta Event の検索コストは無視可 (= `MTrk` 0 先頭 1 つを探すだけ)

### 却下案

- **`sidecar` (= `song.mid` + `song.codetta.meta.json` の 2 ファイル)** — ファイル共有時に分離リスク、 ZIP 圧縮なしでは破綻。 ただし「MIDI を編集する DAW が Text Meta Event を勝手に削除する」 ケース向けに `--extensions sidecar` で optional 提供
- **key=value 形式 (例: `"codetta:master_gain=2.0"`)** — fx チェーン (= 配列) / preset 詳細 (= ネスト) を表現できず JSON inline と比べて利点なし
- **`none` (= 拡張属性を捨てる、 純粋 MIDI)** — round-trip 不成立。 ただし「外部 DAW にだけ渡したい」 ケース向けに `--extensions none` で optional 提供

### `--extensions` モード表

| モード | default | export | import |
|---|---|---|---|
| `text-meta` | ✓ | `MTrk` 0 先頭に JSON inline | `MTrk` 0 先頭の JSON inline を読み戻す |
| `sidecar` | | `.mid` 横に `.codetta.meta.json` を別ファイル出力 | `.mid` 横の `.codetta.meta.json` を読み戻す (無ければ MIDI のみで復元) |
| `none` | | 拡張属性を書かない (= 純粋 GM 互換 MIDI) | Text Meta も sidecar も読まず、 MIDI のみで復元 |

### import 時の fallback 順序 (= 拡張属性復元)

`--extensions text-meta` (default) で import する場合:

1. `MTrk` 0 先頭の Text Meta Event に codetta JSON があれば採用 (= 完全復元)
2. 無ければ `<basename>.codetta.meta.json` (= sidecar) を探し、 あれば採用
3. 無ければ MIDI のみで復元 (= `master_gain: 1.0` default、 `fx: []`、 `instrument.params.preset` は Program Change 由来のみ、 `tracks[].name` は `MTrk` name event があればそれ、 無ければ `"Channel {N}"`)
4. fallback したら出力 JSON の `extensions_recovered` フィールドにどこまで復元できたかを返す (= `["master_gain", "fx", "soundfont_params"]` 全 3 種を独立 boolean で報告)

`--extensions sidecar` / `--extensions none` 指定時は fallback せず、 そのモードで明示された読み方だけ行う。

## GM Program → SF2 preset 自動マッピング

### import (= 主戦場)

各 melodic channel の最終 Program Change 値 (= 0-127) を SF2 preset としてそのまま採用、 `bank: 0` 固定。

ドラム channel 10 は Program Change を無視して `bank: 128, preset: 0` 固定 (= ただし非標準 drum kit の Program Change があればその値を `preset` に採用、 上の「drum channel 10 の扱い」 参照)。

#### マッピング失敗時の fallback

「SF2 にその preset が無い」 ケース (= 指定された SF2 が GM 互換でない、 一部 preset が欠けている SF2 等):

- **採用**: **preset 0 (= Acoustic Grand Piano / bank 0) にフォールバック + warning**
  - import の出力 JSON に warning を含める: `{ "warnings": [{ "type": "PRESET_NOT_FOUND", "channel": 5, "requested_preset": 81, "fallback": 0 }] }`
- 却下案:
  - **channel skip**: その channel を track 化しない → 音が「全くない」 状態は復元として問題、 ユーザーは後から `set_instrument` する方が直感的
  - **エラー化**: import 中断 → LLM が「とりあえず開いて編集する」 ワークフローを阻害

drum channel 10 で `bank: 128, preset: X` が見つからない場合も同様に preset 0 (= Standard Drum Kit) に fallback + warning。

採用理由:

- GM 互換 SF2 (= bundle 予定の GeneralUser GS / 一般的な FluidR3 等) ではほぼ全 preset が揃う、 失敗は edge case
- preset 0 fallback でユーザーは「音が鳴る何か」 を得て、 後で `set_instrument` で目的の preset に差替えできる (= LLM 経由でも同じ流れ)
- silent failure を避け warning で明示することで、 round-trip 後に意図しない preset 差替えが起きていることをユーザー / LLM が気付ける

### export (= 補助的)

`.codetta` の `instrument.params.preset` (= 0-127) をそのまま Program Change として書く。 `bank` 値は MIDI 標準では bank select (CC0 / CC32) で書けるが、 codetta export では:

- `bank: 0` (= melodic) → bank select を書かない (= GM default の bank 0)
- `bank: 128` (= drum) → ch10 に書く + bank select は書かない (= GM ch10 = drum bank が暗黙)
- それ以外の bank (= 非 GM 互換 SF2 で使う、 Phase 3 ではほぼ使わない想定) → CC0 (= bank MSB) として bank 値の上位 7 bit を書く、 CC32 (= bank LSB) は 0 固定

`text-meta` 経由で `instrument.params.bank` の正確な値は復元されるので、 bank select は GM 互換性のため最低限書くだけで OK。

## 内蔵 synth (0.1 legacy) の扱い

`.codetta` 側は schema 0.2 (= soundfont 一本) が前提。 Phase 2 (= CDT-NN 内蔵 synth 削除) 完走後の状態で MIDI export が動く。

### 0.1 ファイルを export する場合

- export 前に **implicit migrate** (= 0.1 → 0.2 を in-memory で実行) を通す、 ディスク上の `.codetta` は書き換えない
- migrate の LUT は `codetta migrate` subcommand と共通 (= 02-project-format.md L92、 03-cli.md L474、 別 Issue で実装)
  - 例: `saw_lead` → preset 81 / `drum_kit (kit=808)` → preset 0 / bank 128 等
- export の出力 JSON に warning を含める: `{ "warnings": [{ "type": "LEGACY_VERSION_MIGRATED", "from": "0.1", "to": "0.2" }] }`

### 却下案

- **0.1 内蔵 synth → GM Program 推測フォールバック (= export 経路独自)** — migrate LUT と二重実装になり、 微妙に異なる結果が出るリスク (= migrate 経路と export 経路で「`saw_pad` がどの preset になるか」 が食い違うと user confusion)
- **0.1 export を block** — 「とりあえず DAW で開きたい」 ニーズに応えられない

採用理由:

- migrate を 1 元化、 export はその後段で動くだけ
- ユーザー側の操作で「export 前に `codetta migrate` を実行する」 と等価
- implicit migrate の挙動を warning で明示することで、 「いつのまにか 0.1 → 0.2 に変わっていた」 サプライズを避ける

## MIDI export の信号フロー

```mermaid
flowchart TD
    A[.codetta JSON load] --> B{version 0.2?}
    B -->|yes| C[normalize: drum 要素名キー → MIDI 番号]
    B -->|no, 0.1| M[in-memory migrate 0.1 → 0.2]
    M --> C
    C --> D[channel assign: tracks[] → ch1-9, 11-16 + drum→ch10]
    D --> E{track count <= 16 + drum?}
    E -->|no| ERR[TRACK_LIMIT_EXCEEDED error]
    E -->|yes| F[MTrk 0: tempo / time sig / text-meta]
    F --> G[MTrk 1..N: Program Change + CC7/10 + note on/off]
    G --> H[midly で SMF Type 1 として serialize]
    H --> I[.mid 出力]
    F -.->|extensions=text-meta| TM[JSON inline を FF 01 で書く]
    F -.->|extensions=sidecar| SC[.codetta.meta.json を別ファイル出力]
    F -.->|extensions=none| NONE[拡張属性スキップ]
```

## MIDI import の信号フロー

```mermaid
flowchart TD
    A[.mid load] --> B[midly で SMF parse]
    B --> C{Type 0 or 1?}
    C -->|Type 0| D0[全 channel を 1 MTrk から分離]
    C -->|Type 1| D1[各 MTrk を走査、 channel 集合を作る]
    D0 --> E[channel set 確定]
    D1 --> E
    E --> F{ch10 あり?}
    F -->|yes| G[drum track として bank=128 で展開]
    F -->|no| H[melodic 用 channel 走査]
    G --> H
    H --> I[Program Change 最終値 → preset]
    I --> J{SF2 に preset 存在?}
    J -->|no| W[preset 0 fallback + warning]
    J -->|yes| K[preset 採用]
    W --> L
    K --> L[note on/off → notes[]]
    L --> EXT{extensions mode}
    EXT -->|text-meta| TM[MTrk 0 先頭の Text Meta から JSON 復元]
    EXT -->|sidecar| SC[.codetta.meta.json を読む]
    EXT -->|none| NONE[拡張属性復元せず default 値]
    TM --> M
    SC --> M
    NONE --> M[最終 Song オブジェクト構築]
    M --> N[.codetta JSON 書き出し]
```

## 実装メモ (= CDT-3 / CDT-4 着手時の参考)

### crates/codetta-core/src/midi/ 構成 (予定)

```
crates/codetta-core/src/midi/
├── mod.rs           # pub fn export_song / import_midi の入口
├── export.rs        # Song → midly::Smf 変換 (channel 割当 / event 生成 / text-meta 埋め込み)
├── import.rs        # midly::Smf → Song 変換 (channel 集約 / Program Change 解釈 / drum 展開 / extensions 復元)
├── gm.rs            # GM Drum Map (= kick=36, snare=38, ...) と要素名キー双方向 LUT
├── extensions.rs    # Text Meta Event JSON 埋め込み / sidecar JSON I/O
└── error.rs         # MidiError (TRACK_LIMIT_EXCEEDED / MULTIPLE_DRUM_TRACKS_NOT_SUPPORTED / PRESET_NOT_FOUND / 等)
```

### midly crate の使い方

- `midly::Smf` で SMF Type 0 / Type 1 両対応の read / write
- `midly::TrackEvent` の `delta` は **tick delta**、 codetta 内部の beat ベースとの変換は `tick = round(beat * ppq)` で統一
- ノート on/off の対応付けは midly 側がやってくれないので、 import では自前で「channel + key で stack 管理」 する必要あり (= 既知の midly limitation)

### Phase 3 着手時のテスト計画

#### round-trip テスト (必須)

`docs/examples/round-trip/cyber-battle.codetta` (= 06-examples.md の Phase 3 fixture 候補) を用意し:

1. `export-midi` で `.mid` 出力
2. `import-midi` で `.codetta` 再生成
3. 意味的同値を assert:
   - `version` / `metadata` (= `bpm`, `time_signature`, `master_gain`, `key`, `tags`) 一致
   - `tracks[]` の各 track で `id` / `name` / `instrument` / `fx` / `volume` / `pan` 一致
   - `notes[]` の `t` / `pitch` (= 数値正規化後) / `dur` / `vel` 一致
   - drum track の `pitch` は数値固定 (= 要素名キーは round-trip で数値化)
4. 三度回し (= 2 を経て 3 度目に再生成しても 2 と完全一致、 = 関数の fixed point)

#### 個別 spec テスト

- channel 割当: 5 track (= 4 melodic + 1 drum) を export して ch1, ch2, ch3, ch4, ch10 が使われることを assert
- track 17 個 = `TRACK_LIMIT_EXCEEDED` エラー
- multiple drum track = `MULTIPLE_DRUM_TRACKS_NOT_SUPPORTED` エラー
- 拡張属性 fallback: `text-meta` を意図的に削った `.mid` を import → sidecar を探す → 無ければ MIDI のみで復元
- preset fallback: SF2 に preset 81 が無い fixture を用意 → import で preset 0 fallback + warning

### 既存実装との接点 (= 着手時の影響範囲)

- `crates/codetta-core/src/lib.rs` に `pub mod midi;` を追加
- CLI: `crates/codetta-cli/src/main.rs` に `export-midi` / `import-midi` subcommand を追加 (= clap derive)
- MCP: `mcp-server/src/index.ts` に `export_midi` / `import_midi` tool を追加 (= CLI spawn 経由、 既存 tool と同じパターン)
- validate: `codetta validate` は MIDI 関連検証を追加しない (= MIDI は input/output 経路、 .codetta の検証は別)
- `Cargo.toml` workspace に `midly = "0.5"` (現行最新確認は着手時に) を追加

## オープンクエスチョン (= Phase 5+ で再検討)

- [ ] **tempo 変化 (= テンポトラック)** — 02-project-format.md「拡張ポイント」 でも Phase 5+ 拡張。 MIDI 側は `FF 51` を複数置けば対応可、 codetta スキーマ拡張と同期で実装
- [ ] **拍子変化** — 同上、 02 と同期
- [ ] **CC オートメーション (= volume / pan / cutoff の経時変化)** — 02 の `tracks[].automation[]` 拡張時。 MIDI 側は CC event 連続で表現
- [ ] **マーカー (= A メロ / サビ等)** — 02 の `markers[]` 拡張時。 MIDI 側は `FF 06` Marker Meta Event
- [ ] **パターン (= ループ可能単位の再利用)** — 02 の `patterns[]` 拡張時。 MIDI 側は標準対応なし → 拡張属性 JSON に埋め込む形になる見込み
- [ ] **import 時の `<basename>.codetta.meta.json` 命名規則** — 拡張子は `.codetta.meta.json` で確定したが、 `song.mid` から `song.codetta.meta.json` (= 拡張子完全置換) か `song.mid.meta.json` (= 拡張子追加) かは着手時に確認 → 採用は **`<basename>.codetta.meta.json`** (= 拡張子完全置換、 同名 `.codetta` ファイルと同じ位置に置けるため diff が見やすい)

## 決着済 (本 ADR で確定したもの)

- [x] channel ↔ track マッピング規則 → **A 案 (1 track = 1 channel、 順序固定)、 drum は ch10 専用**
- [x] drum channel (MIDI 10) の扱い → **export 時 GM Drum Map で MIDI 番号展開、 import 時は数値固定 (= round-trip 優先)、 要素名キー逆変換は `--humanize-drum-keys` で optional**
- [x] 内蔵 synth (0.1 legacy) の MIDI export → **export 前 implicit migrate (= 0.1 → 0.2)、 GM 推測フォールバックを export 経路独自に持たない**
- [x] schema 拡張属性 round-trip → **`text-meta` (= `MTrk` 0 先頭 1 つの Text Meta Event に JSON inline) を default、 sidecar / none は optional**
- [x] GM Program → SF2 preset 自動マッピング失敗時 → **preset 0 fallback + warning** (= silent drop / channel skip / エラー化は却下)
- [x] track 16 channel 超過 → **`TRACK_LIMIT_EXCEEDED` エラーで abort** (= 自動分割 / 自動 merge しない)
- [x] 複数 drum track → **`MULTIPLE_DRUM_TRACKS_NOT_SUPPORTED` エラーで abort**
- [x] MIDI Type → **export は Type 1 固定、 import は Type 0 / Type 1 両対応**
- [x] PPQ default → **480 (`--ppq` で上書き可)**
- [x] `--extensions` default → **`text-meta`**
- [x] sidecar JSON 命名 → **`<basename>.codetta.meta.json`** (= 拡張子完全置換)
- [x] import 時の sidecar fallback → **text-meta なし → sidecar 探索 → なければ MIDI のみで復元**、 `extensions_recovered` でどこまで復元できたか報告

## 関連ドキュメント

- [00-vision.md](00-vision.md) — Phase 3 の位置付け
- [01-architecture.md](01-architecture.md) — `midi/` ディレクトリ配置、 `midly` 依存
- [02-project-format.md](02-project-format.md) — 拡張属性の元定義 (`master_gain` / fx / `instrument.params`)
- [03-cli.md](03-cli.md) — `export-midi` / `import-midi` subcommand
- [04-mcp.md](04-mcp.md) — `export_midi` / `import_midi` MCP tool
- [06-examples.md](06-examples.md) — round-trip fixture 計画 (`midi-roundtrip-demo`)
- [07-soundfont.md](07-soundfont.md) — GM Drum Map (= 要素名キー ↔ MIDI 番号) と GM preset カタログ
- 09-distribution.md — bundle SF2 配布 (Phase 4、 import の `--sf2` 省略時挙動に影響)
