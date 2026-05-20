//! `codetta import-midi` / `codetta export-midi` を結合した round-trip 検証 (CDT-4)。
//!
//! ADR: docs/design/08-midi.md L361-L380 「round-trip テスト (必須)」
//!
//! - export → import で意味的同値 (= JSON shape の重要 field 一致)
//! - 三度回し (= 1 回目 import 後の Song を再 export → 再 import で固定点)
//! - 拡張属性 (master_gain / fx / preset 詳細) の round-trip 確認

use std::path::PathBuf;

use assert_cmd::Command;
use serde_json::{json, Value};

fn unique_tmpdir(stem: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut p = std::env::temp_dir();
    p.push(format!(
        "codetta-midi-rt-{stem}-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn write_song(path: &std::path::Path, song: &Value) {
    let mut bytes = serde_json::to_vec_pretty(song).unwrap();
    bytes.push(b'\n');
    std::fs::write(path, bytes).unwrap();
}

fn run_export(input: &std::path::Path, output: &std::path::Path) -> Value {
    let assert = Command::cargo_bin("codetta")
        .unwrap()
        .args([
            "export-midi",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--force",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    serde_json::from_str(stdout.trim()).expect("CLI stdout JSON (export)")
}

fn run_import(input: &std::path::Path, output: &std::path::Path) -> (Value, Value) {
    let assert = Command::cargo_bin("codetta")
        .unwrap()
        .args([
            "import-midi",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--force",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let payload: Value = serde_json::from_str(stdout.trim()).expect("CLI stdout JSON (import)");
    let written = std::fs::read(output).expect("output written");
    let song: Value = serde_json::from_slice(&written).expect("output JSON");
    (payload, song)
}

#[test]
fn export_import_round_trip_preserves_song() {
    let dir = unique_tmpdir("rt");
    let input = dir.join("orig.codetta");
    let mid = dir.join("orig.mid");
    let imported = dir.join("imported.codetta");

    let song = json!({
        "version": "0.2",
        "metadata": {
            "name": "rt",
            "bpm": 140,
            "key": "Am",
            "time_signature": [4, 4],
            "master_gain": 2.0,
            "tags": ["rt-test"],
        },
        "tracks": [
            {
                "id": "lead",
                "name": "Saw Lead",
                "instrument": {
                    "type": "soundfont",
                    "params": { "file": "GeneralUser-GS.sf2", "preset": 81, "bank": 0 }
                },
                "volume": 0.8,
                "pan": 0.0,
                "fx": [{ "type": "reverb", "mix": 0.2 }],
                "notes": [
                    { "t": 0.0,  "pitch": 60, "dur": 1.0, "vel": 100 },
                    { "t": 1.0,  "pitch": 64, "dur": 1.0, "vel":  90 }
                ]
            },
            {
                "id": "drums",
                "name": "Drums",
                "instrument": {
                    "type": "soundfont",
                    "params": { "file": "GeneralUser-GS.sf2", "preset": 0, "bank": 128 }
                },
                "volume": 0.9,
                "pan": 0.0,
                "notes": [
                    { "t": 0.0,  "pitch": "kick",  "dur": 0.25, "vel": 110 },
                    { "t": 0.5,  "pitch": "snare", "dur": 0.25, "vel": 115 }
                ]
            }
        ]
    });
    write_song(&input, &song);

    run_export(&input, &mid);
    let (import_payload, rt_song) = run_import(&mid, &imported);

    assert_eq!(
        import_payload["extensions_recovered"]["source"],
        "text-meta"
    );
    assert_eq!(import_payload["extensions_recovered"]["master_gain"], true);
    assert_eq!(import_payload["extensions_recovered"]["fx"], true);
    assert_eq!(
        import_payload["extensions_recovered"]["soundfont_params"],
        true
    );

    assert_eq!(rt_song["version"], "0.2");
    assert_eq!(rt_song["metadata"]["bpm"], 140);
    assert_eq!(rt_song["metadata"]["key"], "Am");
    assert_eq!(rt_song["metadata"]["tags"], json!(["rt-test"]));
    let mg = rt_song["metadata"]["master_gain"].as_f64().unwrap();
    assert!((mg - 2.0).abs() < 1e-3, "master_gain={mg}");

    let tracks = rt_song["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 2);

    // lead
    assert_eq!(tracks[0]["id"], "lead");
    assert_eq!(tracks[0]["name"], "Saw Lead");
    assert_eq!(tracks[0]["instrument"]["type"], "soundfont");
    assert_eq!(tracks[0]["instrument"]["params"]["preset"], 81);
    assert_eq!(tracks[0]["instrument"]["params"]["bank"], 0);
    let fx = tracks[0]["fx"].as_array().unwrap();
    assert_eq!(fx.len(), 1);
    assert_eq!(fx[0]["type"], "reverb");

    let lead_notes = tracks[0]["notes"].as_array().unwrap();
    assert_eq!(lead_notes.len(), 2);
    assert_eq!(lead_notes[0]["pitch"], 60);
    assert_eq!(lead_notes[1]["pitch"], 64);

    // drums (ADR L120: 要素名キーは round-trip で MIDI 番号に正規化)
    assert_eq!(tracks[1]["id"], "drums");
    assert_eq!(tracks[1]["instrument"]["params"]["bank"], 128);
    let drum_notes = tracks[1]["notes"].as_array().unwrap();
    assert_eq!(drum_notes.len(), 2);
    assert_eq!(drum_notes[0]["pitch"], 36); // kick
    assert_eq!(drum_notes[1]["pitch"], 38); // snare

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&mid);
    let _ = std::fs::remove_file(&imported);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn three_pass_round_trip_is_fixed_point() {
    // ADR L372 「三度回し」: import → export → import で固定点に到達することを確認。
    let dir = unique_tmpdir("fixed-point");
    let input = dir.join("p0.codetta");
    let mid_1 = dir.join("p1.mid");
    let imp_1 = dir.join("p1.codetta");
    let mid_2 = dir.join("p2.mid");
    let imp_2 = dir.join("p2.codetta");

    write_song(
        &input,
        &json!({
            "version": "0.2",
            "metadata": {
                "name": "fp",
                "bpm": 120,
                "time_signature": [4, 4],
                "master_gain": 1.0
            },
            "tracks": [{
                "id": "lead",
                "name": "lead",
                "instrument": {
                    "type": "soundfont",
                    "params": { "file": "GeneralUser-GS.sf2", "preset": 81, "bank": 0 }
                },
                "volume": 0.8,
                "pan": 0.0,
                "notes": [
                    { "t": 0.0,  "pitch": 60, "dur": 1.0, "vel": 100 },
                    { "t": 1.5,  "pitch": 67, "dur": 0.5, "vel":  90 }
                ]
            }]
        }),
    );

    run_export(&input, &mid_1);
    let (_, mut p1) = run_import(&mid_1, &imp_1);
    run_export(&imp_1, &mid_2);
    let (_, mut p2) = run_import(&mid_2, &imp_2);

    // import 側で `metadata.name` を path stem から再生成するため毎回変わる (= 期待動作)。
    // 比較から除外して、 ノート / トラック / それ以外の metadata が完全一致することを確認する。
    p1["metadata"]["name"] = json!("<normalized>");
    p2["metadata"]["name"] = json!("<normalized>");
    assert_eq!(
        p1, p2,
        "round-trip should reach a fixed point after one pass"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
