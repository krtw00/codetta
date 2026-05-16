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

## スコープ (Phase 2 = song 内で SF2 を実際に鳴らす)

- [x] `Instrument::SoundFont` の render path 統合 (`render_soundfont_track` を track 単位で呼ぶ)
- [x] `KNOWN_INSTRUMENT_TYPES` に `"soundfont"` 追加 + params (file/preset/bank) 検証
- [x] `instrument_catalog()` に SF2 entry 追加 (MCP server は CLI 経由なので noop で伝搬)
- [x] `$CODETTA_SOUNDFONT_DIR` (default `$HOME/Music/sf2/`) で相対 path 解決
- [x] 解決後 path が見つからない場合 `SOUNDFONT_FILE_NOT_FOUND` validation error
- [x] env-gated 統合テスト: 多重 note の track render + render-level smoke

## スコープ (Phase 3 = 検索 / discovery、 未着手)

- [ ] `list_soundfont_presets(file)` tool / `codetta://soundfonts/` resource
- [ ] README に SF2 download 手順 (推奨 OSS SF2 一覧) 追加
- [ ] `Arc<SoundFont>` キャッシュ共有 (同一 SF2 を複数 track で参照する場合の load 重複回避)

## Instrument スキーマ (Phase 2 で実装済み)

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

## Render path (Phase 2 実装)

既存 melodic は `Box<dyn Fn(f, h, adsr) -> Vec<f32>>` の **per-voice 独立 closure** だが、
rustysynth は **synthesizer instance が channel/voice state を保持** するので per-note 独立 render ができない。

→ SF2 トラックは **専用の render path** (`render_soundfont_track`) を持つ:
1. track 開始時に SF2 を load して `Synthesizer` を 1 つ生成 (`open_synth`)
2. note を時刻順 sort し、 `(sample_idx, on/off, key, vel)` の event 列に展開
3. `note_on` / `note_off` を発火しつつ、 イベント間を `synth.render(L_slice, R_slice)` で埋める
4. 末尾は `total_samples` まで render し切り (release tail 自然減衰)
5. 出力 stereo buffer に `track.volume * pan_gains` を per-channel 乗算して master へ加算

`render::render_to_buffer` 内の dispatch は `kind == "soundfont"` で分岐 (drum / melodic と並列)。
SF2 load 失敗時は track をスキップして `eprintln` 警告 (validate で報告される責務)。

ポリフォニーは rustysynth デフォルト (64 voice)。 必要なら `SynthesizerSettings::maximum_polyphony` で調整。

## License

- **Codetta repo / dist には SF2 を含めない**。 ユーザー自身が download
- 推奨 OSS SF2:
  - **GeneralUser GS** (Schristian Collins, free for any use, ~30MB) — GM/GS 235 preset、 高品質
  - **TimGM6mb** (GPL, ~6MB) — 軽量、 PoC テスト向け
  - **FluidR3_GM** (MIT, ~140MB) — クラシック標準
- README で download 手順を案内 (Phase 3)
- rustysynth: MIT

## Risks / 未解決

- **SF3 (OGG Vorbis 圧縮)** は rustysynth 1.3 では未対応。 SF2 のみ
- **sample rate 不一致** — Phase 2 では 44.1kHz 固定。 rustysynth は内部リサンプルする (SF2 sample rate と SynthesizerSettings の rate を変えれば追従)
- **メモリ使用量** — 大きい SF2 (~150MB) を load した場合の挙動。 同一 SF2 を複数 track で参照する場合は **track ごとに重複 load される** (Phase 3 で `Arc<SoundFont>` キャッシュ検討)
- **音源切替の cost** — 同一 SF2 の中で preset を切替えるのは `process_midi_message(channel, 0xC0, program, 0)` で軽量 (cf. SF2 load は重い)
- **同時刻 note の event 順** — `start_sample` 同点なら off → on の順で sort (= レガート的に渡るが、 楽譜的に厳密ではない)。 必要なら note の `id` などで決定論的順序を入れる
- **Codetta CLI を経由する MCP server (TypeScript)** は `instrument_catalog()` JSON 経由で SF2 entry を自動取得 (TypeScript 側完全ノータッチ達成)

## PoC の検証方法

```bash
# 前提: ~/Music/sf2/GeneralUser-GS-v1.471.sf2 が存在
CODETTA_TEST_SF2="$HOME/Music/sf2/GeneralUser-GS-v1.471.sf2" \
  cargo test --workspace soundfont
```

期待: SF2 から MIDI key 60 (C4) を 1 秒 render して、 stereo buffer の長さと振幅が想定範囲に収まっていること。

## Phase 2 の検証方法

```bash
# CLI で SF2 track を含む song を作って render
codetta new sf2.codetta --bpm 100 --force
codetta add-track sf2.codetta --id piano --name Piano \
  --instrument soundfont \
  --params-json '{"file":"GeneralUser-GS-v1.471.sf2","preset":0}'
echo '[{"t":0,"pitch":"C4","dur":1},{"t":1,"pitch":"E4","dur":1},{"t":2,"pitch":"G4","dur":1}]' > notes.json
codetta set-notes sf2.codetta --track piano --notes-file notes.json
codetta validate sf2.codetta   # SOUNDFONT_FILE_NOT_FOUND が出なければ OK
codetta render sf2.codetta --output sf2.wav
```

期待: 3 ノートの piano arpeggio が `sf2.wav` に WAV として書き出される (~4秒)。 file が見つからなければ `validate` で `SOUNDFONT_FILE_NOT_FOUND` が報告される。
