# Codetta — 内蔵音源仕様

> 軽量な全 DSP 合成音源 (内蔵) + 外部 SoundFont (optional 拡張) のハイブリッド。
> 内蔵音源はサイバー感 / 電子音 / チップチューン系を主戦場とし、
> 生楽器の精密エミュレーションが必要なら 07-soundfont.md の SF2 拡張を使う。

## 設計方針

1. **既定は全合成 (No samples)** — Codetta 本体は sample / 大きいバイナリを bundle しない。 配布サイズ最小化、 ライセンス問題回避
2. **SF2 (SoundFont) を optional 拡張として参照可能** — ユーザーが path 指定で SF2 を持ち込めば sample-based 音源 (生音 / GM 互換) を render に使える。 内蔵音源 (全合成) と共存。 詳細: [07-soundfont.md](./07-soundfont.md)
3. **軽量** — 1 トラック ≤ 1ms / バッファ (内蔵音源)。 SF2 経路は SoundFont の規模に依存
4. **LLM フレンドリーなパラメータ命名** — `cutoff_hz` / `attack_sec` 等、 単位を名前に含める
5. **デフォルト値で「それなりに鳴る」** — 全パラメータ省略で意味のある音が出る
6. **Phase 0 は最小セット** — 拡張は需要を見てから

## シンセエンジンの全体像

### 1 ボイスの信号フロー

```mermaid
flowchart LR
    Note["MIDI Note<br/>(pitch, vel)"] --> Osc["Oscillator<br/>(sin / saw / square / triangle)"]
    Note --> Env["ADSR Envelope"]
    Osc --> Mul["× (gain)"]
    Env --> Mul
    Mul --> Filt["Filter<br/>(lowpass / highpass)"]
    Filt --> VelGain["× velocity / 127"]
    VelGain --> Out["voice out"]
```

### トラック全体の信号フロー

```mermaid
flowchart LR
    V1[voice 1] --> Mix["Σ (mix voices)"]
    V2[voice 2] --> Mix
    Vn[voice N] --> Mix
    Mix --> Fx1["fx[0]"]
    Fx1 --> Fx2["fx[1]"]
    Fx2 --> FxN["fx[N]"]
    FxN --> Vol["× track.volume"]
    Vol --> Pan["pan (L/R)"]
    Pan --> TrackOut["track out"]
```

### 楽曲全体

各トラック出力をミックスバスで合算 → 必要なら最終リミッタ → WAV 書き出し。

```mermaid
flowchart LR
    T1[track 1] --> Bus["master bus<br/>(simple sum)"]
    T2[track 2] --> Bus
    Tn[track N] --> Bus
    Bus --> Lim["soft limiter<br/>(clipping 防止)"]
    Lim --> Wav["WAV 出力<br/>(44.1kHz / 16bit)"]
```

## 設定値

| 項目 | 値 | 備考 |
|---|---|---|
| サンプルレート | 44100 Hz | 標準。 48000Hz / 96000Hz は Phase 1+ で検討 |
| ビット深度 | 16-bit signed PCM | 標準。 24-bit は Phase 1+ |
| チャンネル | Stereo (2ch) | パン処理を簡略化のため常に stereo |
| ポリフォニー (1 トラック) | 16 voices | 上限超過時は古い voice を steal |
| マスター soft limiter | tanh 系 | clipping 防止のみ、 マスタリング目的ではない |

## オシレータ

### `sin`

純音 (倍音なし)。 サブベース / パッド / FM の素材に。

$$ y(t) = \sin(2\pi f t) $$

**params:** (なし)

### `saw` / `saw_lead`

ノコギリ波。 リード / ベース / パッド向け。 倍音豊富。

$$ y(t) = 2 \left( \frac{t f \bmod 1}{1} \right) - 1 $$

**エイリアシング対策:** Phase 0 では PolyBLEP (Polynomial Band-Limited Step) を使用。
fundsp 採用なら `saw_hz()` がデフォルトで対応。

### `square` / `square_bass`

矩形波。 8bit 風 / チップチューン / ベース。 デューティ比可変。

$$ y(t) = \begin{cases} +1 & \text{if } (t f \bmod 1) < \text{pw} \\ -1 & \text{otherwise} \end{cases} $$

**params:**
| 名前 | 型 | 範囲 | デフォルト | 説明 |
|---|---|---|---|---|
| `pulse_width` | float | 0.1 - 0.9 | 0.5 | デューティ比 (0.5 = 矩形、 0.1 = 細パルス) |

### `triangle`

三角波。 柔らかい音、 笛系。

$$ y(t) = 2 \left| 2 \left( (t f \bmod 1) - 0.5 \right) \right| - 1 $$

### `saw_pad`

`saw` 3 つを detune して重ねたもの。 厚みあるパッド。

| 名前 | 型 | 範囲 | デフォルト | 説明 |
|---|---|---|---|---|
| `detune_cents` | float | 0 - 50 | 10 | 各 saw のずれ (1 セント = 1/100 半音) |

## ADSR エンベロープ

```mermaid
flowchart LR
    A[Attack<br/>0 → 1] --> D[Decay<br/>1 → sustain]
    D --> S[Sustain<br/>(note 持続中)]
    S --> R[Release<br/>sustain → 0]
```

**params (全 instrument 共通):**

| 名前 | 型 | 範囲 | デフォルト | 説明 |
|---|---|---|---|---|
| `attack` | float | 0 - 10 | 0.01 | 立ち上がり時間 (秒) |
| `decay` | float | 0 - 10 | 0.1 | sustain への到達時間 (秒) |
| `sustain` | float | 0 - 1 | 0.7 | 持続レベル (0-1) |
| `release` | float | 0 - 10 | 0.2 | リリース時間 (秒) |

**曲線:** 全て指数 (人間の聴感に合わせる)。 リニアは Phase 1+ でオプション追加。

## フィルタ

### `lowpass` / `highpass`

State Variable Filter (SVF) を採用。 cutoff と Q を独立に制御可。

| 名前 | 型 | 範囲 | デフォルト | 説明 |
|---|---|---|---|---|
| `cutoff` (Hz) | float | 20 - 20000 | 1000 | カットオフ周波数 |
| `q` | float | 0.5 - 10 | 1.0 | レゾナンス (高いほど狭帯域強調) |

**Phase 0 では filter envelope なし**。 Phase 1 で `filter_env_amount` / `filter_env_attack` 等を追加。

## エフェクト

### `lowpass` / `highpass` (track fx)

オシレータ直後のフィルタとは別に、 トラックエフェクトとしても利用可能。 実装は同じ SVF。

### `delay`

ディレイライン + フィードバック。

| 名前 | 型 | 範囲 | デフォルト | 説明 |
|---|---|---|---|---|
| `time` | string \| float | "1/16" - "1" or 0.01 - 2.0 | "1/8" | ディレイ時間。 BPM 同期 ("1/4" 等) or 秒 |
| `feedback` | float | 0 - 0.95 | 0.3 | フィードバック量 (0.95 超は発振防止のため clamp) |
| `mix` | float | 0 - 1 | 0.25 | ドライ / ウェット比 |

### `reverb`

Schroeder reverb (4 comb + 2 allpass) を基本実装。

| 名前 | 型 | 範囲 | デフォルト | 説明 |
|---|---|---|---|---|
| `size` | float | 0 - 1 | 0.5 | 空間の広さ (リバーブ長) |
| `damp` | float | 0 - 1 | 0.5 | 高域減衰 (1 で完全減衰) |
| `mix` | float | 0 - 1 | 0.2 | ドライ / ウェット比 |

### `distortion`

ソフトクリッピング (tanh)。

| 名前 | 型 | 範囲 | デフォルト | 説明 |
|---|---|---|---|---|
| `amount` | float | 0 - 1 | 0.3 | 歪み量 (入力ゲイン) |
| `tone` | float | 0 - 1 | 0.5 | tone (内蔵 lowpass cutoff: 0=暗、 1=明) |

## ドラム音源 (`drum_kit`)

サンプルではなく全合成。 各ドラムは固定の合成レシピ + `kit` で味付け変更。

### `kick` (バスドラム)

```mermaid
flowchart LR
    SinSweep["sin oscillator<br/>(freq: 150Hz → 50Hz)"] --> Mul["×"]
    Env1["pitch envelope<br/>(50ms exp decay)"] --> SinSweep
    Env2["amp envelope<br/>(attack 1ms, decay 200ms)"] --> Mul
    Click["click<br/>(short noise burst)"] --> Sum["+"]
    Mul --> Sum
    Sum --> Out[out]
```

| `kit` | バリエーション |
|---|---|
| `808` | freq 80Hz→40Hz, decay 400ms, click 弱め |
| `909` | freq 200Hz→60Hz, decay 200ms, click 強め |
| `chip` | 単純な square 1 周期 + noise (8bit 風) |

### `snare` (スネア)

```mermaid
flowchart LR
    Noise["white noise"] --> BP["bandpass<br/>(1.5-4kHz)"]
    Sin["sin 200Hz"] --> Sum["+"]
    BP --> Sum
    Sum --> Env["amp envelope<br/>(attack 1ms, decay 150ms)"]
    Env --> Out[out]
```

| `kit` | バリエーション |
|---|---|
| `808` | sin 主体、 noise 控えめ、 decay 長め |
| `909` | noise 強め、 decay 短め、 punch あり |
| `chip` | 短い noise + square click |

### `hh_closed` / `hh_open` (ハイハット)

```mermaid
flowchart LR
    Noise["white noise"] --> HP["highpass<br/>(6kHz)"]
    HP --> Env["amp envelope"]
    Env --> Out[out]
```

| 要素 | `hh_closed` | `hh_open` |
|---|---|---|
| decay | 50ms | 500ms |

### `clap` / `crash` / `ride` / `tom_*`

各要素のレシピは Phase 0 実装時に詳細決定。 基本パターン:

- `clap`: noise burst × 4 (短い間隔で重ねる)
- `crash`: noise + bandpass (4-10kHz) + long decay (2s)
- `ride`: noise + bandpass (5-8kHz) + medium decay (1s) + 周期的アクセント
- `tom_*`: sin sweep + noise + amp envelope (周波数で lo/mid/hi)

## サイバー感プリセット集

ddc / DCG / サイバー系 BGM に使える即興プリセット。 設計ドキュメントとしての参考、 実装時にライブラリ化。

### "cyber_lead" (主旋律)

```json
{
  "instrument": {
    "type": "saw_lead",
    "params": { "attack": 0.005, "decay": 0.05, "sustain": 0.8, "release": 0.15, "filter_cutoff": 1500, "filter_q": 3.0 }
  },
  "fx": [
    { "type": "delay", "time": "1/8", "feedback": 0.35, "mix": 0.3 },
    { "type": "reverb", "size": 0.4, "mix": 0.2 }
  ]
}
```

### "sub_bass" (重低音)

```json
{
  "instrument": {
    "type": "sin",
    "params": { "attack": 0.005, "decay": 0.1, "sustain": 0.95, "release": 0.05 }
  },
  "fx": [
    { "type": "lowpass", "cutoff": 150, "q": 0.7 }
  ]
}
```

### "cyber_arp" (アルペジオ)

```json
{
  "instrument": {
    "type": "square",
    "params": { "attack": 0.001, "decay": 0.05, "sustain": 0.0, "release": 0.05, "pulse_width": 0.3 }
  },
  "fx": [
    { "type": "delay", "time": "1/16", "feedback": 0.5, "mix": 0.4 },
    { "type": "reverb", "size": 0.6, "mix": 0.3 }
  ]
}
```

### "wide_pad" (背景パッド)

```json
{
  "instrument": {
    "type": "saw_pad",
    "params": { "attack": 0.5, "decay": 0.3, "sustain": 0.6, "release": 1.0, "detune_cents": 15 }
  },
  "fx": [
    { "type": "lowpass", "cutoff": 2000, "q": 0.7 },
    { "type": "reverb", "size": 0.9, "damp": 0.6, "mix": 0.5 }
  ]
}
```

## パフォーマンス考慮

### Phase 0 目標

- 3 分の曲 (5 track × 平均 100 notes) をオフラインで 18 秒以内にレンダリング = リアルタイム 10x
- メモリ使用 < 100MB

### 設計上の工夫

- **lock-free** — オーディオ処理スレッドで mutex 取らない (オフラインでは不要、 GUI Phase 3 で必要)
- **ブロック処理** — サンプル単位ではなくバッファ単位 (例: 256 sample / block) で処理
- **SIMD** (Phase 2+) — `wide` crate 等で voice ループを SIMD 化
- **早期終了** — release 完了後の voice は計算スキップ

## `fundsp` 採用判断

**結論: 不採用 (自前実装に統一)**。 Phase 0 first cut でプロトタイプ比較した結果、 自前ループ実装に明確な優位があった。

### 比較結果 (sin + ADSR / 1 voice, 1000 回生成, release build)

| 項目 | 自前 (`synth::manual`) | `fundsp` 版 |
|---|---|---|
| コード行数 | 約 55 行 | 約 45 行 (ADSR ロジック自体は同一) |
| 性能 (M samples/s) | 247 | 200 |
| 依存 crate 数 | 0 追加 | +12 (ahash / hashbrown / funutd / thingbuf 等) |
| LLM 拡張容易性 | 素の Rust ループ、 patch しやすい | ノード合成のため部分書き換えが効きにくい |

### 判断理由

1. **性能**: 自前のほうが 24% 高速。 `fundsp` は per-note のノード構築オーバーヘッドが大きく、 「ノートごとにグラフ生成」 という Codetta 想定の使い方と相性が悪い (`fundsp` は本来 長時間 stream 想定)
2. **コード量は互角**: `sin + ADSR` レベルでは演算子合成の旨味がほぼ出ない。 saw / square / filter / reverb を増やしても、 ADSR ロジック (segment 分岐) は結局同じ量を書く必要がある
3. **LLM 第一原則**: Codetta の DSP は LLM が読んで patch する対象。 素の Rust ループは Codetta 全体の方針と整合 (cutoff_hz / attack_sec 等の命名自由度も高い)
4. **依存最小化**: 「サンプル不要・全 DSP 合成」 「配布サイズ最小化」 の方針と一貫

### 補足

- Phase 0 第一実装の `manual::render_voice` は線形 ADSR セグメント。 指数曲線への置き換えは Phase 1+ で検討
- 将来 Schroeder reverb / saw_pad detune など複雑な信号フローを書く場合でも、 段階的に DSP プリミティブを自前で増やす方針 (`fundsp` 再評価は Phase 2 以降の SIMD 化検討時)

## オープンクエスチョン

- [x] `fundsp` 採用可否 → **不採用**。 自前実装に統一 (上記「`fundsp` 採用判断」参照)
- [ ] サンプルレート: 44100Hz 固定か、 48000Hz もサポートか → 当面 44100Hz
- [x] ステレオパンの法則: linear / -3dB / -4.5dB → -3dB を採用 (Phase 0 first cut で実装済)
- [ ] mid/side 処理: Phase 0 で必要か → 不要、 Phase 2+
- [ ] マスター soft limiter のしきい値 → -0.5dB (Phase 0 first cut で tanh ベースで実装、 -0.5dB 目安)
