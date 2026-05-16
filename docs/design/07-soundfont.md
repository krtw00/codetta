# Codetta — SoundFont (SF2) optional 拡張

> 内蔵音源 (全合成、 [05-sound.md](./05-sound.md)) では届かない 「生音 / GM 互換 / 標準的なソフトシンセ音色」 を、
> ユーザーが SF2 (SoundFont 2) ファイルを持ち込むことで補う optional 拡張。

## 動機

Phase 0 / Phase 1 の内蔵音源は `sin / saw / square / triangle / saw_pad / drum_kit` の synth 系のみ。
生楽器 (ピアノ / 弦 / ブラス / アコギ / 木管 / 合唱 / etc.) は サンプル or 物理モデルが要るが、
- 全合成で精度を出すと実装コストが大きい
- サンプルを Codetta 本体に同梱するとライセンス問題 / 配布サイズ膨張

→ **SoundFont (.sf2) を外部参照で読み込む経路を増やす**。 Codetta 本体は SF2 を bundle せず、
ユーザーが OSS SF2 を path 指定して使う。

## スコープ (Phase 1 = PoC)

- [x] `rustysynth = "1.3"` を `codetta-core` の dependency に追加 (MIT)
- [x] `crates/codetta-core/src/synth/soundfont.rs` に最小 render 関数
  - 引数: SF2 file path, preset, bank, midi key, velocity, hold_sec, sample_rate
  - 返り値: stereo `(Vec<f32>, Vec<f32>)`
- [x] env-gated 単体テスト (`CODETTA_TEST_SF2` が指す SF2 がある場合のみ実行)
- [ ] `Instrument::SoundFont` の render path 統合 → **Phase 2 (次セッション)**
- [ ] `KNOWN_INSTRUMENT_TYPES` / catalog 更新 → Phase 2
- [ ] MCP server (`list_instruments` JSON 結果) に SF2 entry 追加 → Phase 2
- [ ] `list_soundfont_presets(file)` tool / `codetta://soundfonts/` resource → Phase 3

PoC 段階では既存 song 内では SF2 instrument を **まだ使えない** (render dispatch が未統合)。
PoC test が通れば 「rustysynth で SF2 から PCM を取り出せる」 ことが Codetta 内で証明される。

## Instrument スキーマ (Phase 2 で導入予定)

```jsonc
{
  "type": "soundfont",
  "params": {
    "file": "GeneralUser-GS-v1.471.sf2",  // 相対 or 絶対 path
    "preset": 0,                            // GM Program 番号 (0 = Acoustic Grand Piano)
    "bank": 0                               // GM bank (省略時 0)
  }
}
```

## Path 解決

- 絶対 path → そのまま
- 相対 path → `$CODETTA_SOUNDFONT_DIR` (default: `$HOME/Music/sf2/`) 配下として解釈
- 未発見なら `SoundFontFileNotFound { path }` validation error

`$CODETTA_WORKSPACE` (= 既存) と同じ env pattern。 MCP server は環境変数を継承する。

## Render path (Phase 2 設計案)

既存 melodic は `Box<dyn Fn(f, h, adsr) -> Vec<f32>>` の **per-voice 独立 closure** だが、
rustysynth は **synthesizer instance が channel/voice state を保持** するので per-note 独立 render ができない。

→ SF2 トラックは **専用の render path** を作る:
1. track 開始時に SF2 を load して `Synthesizer` を 1 つ生成
2. 全 note を時刻順 sort
3. `note_on` → 区間ごとに `render(&mut left, &mut right)` でサンプルを積む → `note_off` → release tail まで render
4. 出力は stereo buffer (= 既存 mono pipeline と別、 mix 時に合算)

ポリフォニーは `SynthesizerSettings::maximum_polyphony` で制御。

## License

- **Codetta repo / dist には SF2 を含めない**。 ユーザー自身が download
- 推奨 OSS SF2:
  - **GeneralUser GS** (Schristian Collins, free for any use, ~30MB) — GM/GS 235 preset、 高品質
  - **TimGM6mb** (GPL, ~6MB) — 軽量、 PoC テスト向け
  - **FluidR3_GM** (MIT, ~140MB) — クラシック標準
- README で download 手順を案内 (Phase 2 で追加)
- rustysynth: MIT

## Risks / 未解決

- **SF3 (OGG Vorbis 圧縮)** は rustysynth 1.3 では未対応。 SF2 のみ
- **sample rate 不一致** — SF2 内の sample rate と Codetta の出力 sample rate (44.1kHz / 48kHz) が違うとき rustysynth がリサンプルするか確認 (Phase 2)
- **メモリ使用量** — 大きい SF2 (~150MB) を load した場合の挙動。 Phase 2 で `Arc<SoundFont>` キャッシュを project 単位で共有 ?
- **音源切替の cost** — 同一 SF2 の中で preset を切替えるのは `process_midi_message(channel, 0xC0, program, 0)` で軽量 (cf. SF2 load は重い)
- **Codetta CLI を経由する MCP server (TypeScript)** に SF2 instrument を伝える経路は既存の `instrument_catalog()` JSON 出力に entry 追加で完結する見込み (TypeScript 側ノータッチ)

## PoC の検証方法

```bash
# 前提: ~/Music/sf2/GeneralUser-GS-v1.471.sf2 が存在
CODETTA_TEST_SF2="$HOME/Music/sf2/GeneralUser-GS-v1.471.sf2" \
  cargo test --workspace soundfont
```

期待: SF2 から MIDI key 60 (C4) を 1 秒 render して、 stereo buffer の長さと振幅が想定範囲に収まっていること。
