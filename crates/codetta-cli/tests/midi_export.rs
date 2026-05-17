//! `codetta export-midi` の integration test (CDT-4)。
//!
//! - fixture `.codetta` は in-test 構築 (= テスト独立性のため fixture file を repo に持たない)。
//! - CLI を spawn して JSON stdout + 出力 `.mid` を検証する。
//! - round-trip (export → import) は `midi_roundtrip.rs` 側でカバー。
//!
//! ADR: docs/design/08-midi.md

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
        "codetta-midi-export-{stem}-{nanos}-{}",
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

fn run_export_expect_success(input: &std::path::Path, output: &std::path::Path) -> Value {
    let assert = Command::cargo_bin("codetta")
        .unwrap()
        .args([
            "export-midi",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    serde_json::from_str(stdout.trim()).expect("CLI stdout JSON")
}

fn run_export_expect_failure(args: &[&str]) -> Value {
    let assert = Command::cargo_bin("codetta")
        .unwrap()
        .args(args)
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    serde_json::from_str(stdout.trim()).expect("CLI stdout JSON")
}

fn melodic_track(id: &str, preset: u16, notes: Value) -> Value {
    json!({
        "id": id,
        "name": id,
        "instrument": {
            "type": "soundfont",
            "params": { "file": "test.sf2", "preset": preset, "bank": 0 }
        },
        "volume": 0.8,
        "pan": 0.0,
        "notes": notes,
    })
}

fn drum_track(id: &str, notes: Value) -> Value {
    json!({
        "id": id,
        "name": id,
        "instrument": {
            "type": "soundfont",
            "params": { "file": "test.sf2", "preset": 0, "bank": 128 }
        },
        "volume": 0.9,
        "pan": 0.0,
        "notes": notes,
    })
}

fn song_with(tracks: Value) -> Value {
    json!({
        "version": "0.2",
        "metadata": {
            "name": "t",
            "bpm": 120,
            "time_signature": [4, 4],
            "master_gain": 1.0
        },
        "tracks": tracks,
    })
}

#[test]
fn cli_exports_basic_song_with_text_meta_extensions() {
    let dir = unique_tmpdir("basic");
    let input = dir.join("song.codetta");
    let output = dir.join("song.mid");
    let song = song_with(json!([melodic_track(
        "lead",
        81,
        json!([
            { "t": 0.0, "pitch": 60, "dur": 1.0, "vel": 100 },
            { "t": 1.0, "pitch": 64, "dur": 1.0, "vel": 100 },
        ]),
    ),]));
    write_song(&input, &song);

    // SF2 file 存在検証は通常 io::load 経由で行われるが、 codetta 側の
    // soundfont validate は実 SF2 file をチェックする。 そのため、 export 前に
    // io::load が呼ばれない経路 (= raw JSON load) で逃げる必要がある。
    // export-midi は raw JSON load 経路なので、 SF2 検証はスキップされる。
    let payload = run_export_expect_success(&input, &output);
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["format"], "Type 1");
    assert_eq!(payload["ppq"], 480);
    assert_eq!(payload["track_count"], 1);
    assert_eq!(payload["text_meta_written"], true);

    assert!(output.exists(), "output .mid must be written");
    let bytes = std::fs::read(&output).expect("read .mid");
    let smf = midly::Smf::parse(&bytes).expect("parse .mid");
    assert_eq!(smf.tracks.len(), 2); // meta + 1 channel

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn cli_refuses_to_overwrite_without_force() {
    let dir = unique_tmpdir("overwrite");
    let input = dir.join("song.codetta");
    let output = dir.join("song.mid");
    write_song(&input, &song_with(json!([])));
    std::fs::write(&output, b"placeholder").unwrap();

    let payload = run_export_expect_failure(&[
        "export-midi",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
    ]);
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["errors"][0]["code"], "FILE_EXISTS");
    assert_eq!(std::fs::read(&output).unwrap(), b"placeholder");

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn cli_rejects_more_than_15_melodic_tracks() {
    let dir = unique_tmpdir("limit");
    let input = dir.join("song.codetta");
    let output = dir.join("song.mid");
    let tracks: Vec<Value> = (0..16)
        .map(|i| melodic_track(&format!("mel-{i}"), 0, json!([])))
        .collect();
    write_song(&input, &song_with(Value::Array(tracks)));

    let payload = run_export_expect_failure(&[
        "export-midi",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
    ]);
    assert_eq!(payload["errors"][0]["code"], "MIDI_TRACK_LIMIT_EXCEEDED");
    assert_eq!(
        payload["errors"][0]["context"]["excess_track_ids"],
        json!(["mel-15"])
    );
    assert!(!output.exists());

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn cli_rejects_multiple_drum_tracks() {
    let dir = unique_tmpdir("multi-drum");
    let input = dir.join("song.codetta");
    let output = dir.join("song.mid");
    write_song(
        &input,
        &song_with(json!([
            drum_track("drums-a", json!([])),
            drum_track("drums-b", json!([])),
        ])),
    );

    let payload = run_export_expect_failure(&[
        "export-midi",
        input.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
    ]);
    assert_eq!(payload["errors"][0]["code"], "MIDI_MULTIPLE_DRUM_TRACKS");
    assert_eq!(
        payload["errors"][0]["context"]["drum_track_ids"],
        json!(["drums-a", "drums-b"])
    );

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn cli_drum_element_keys_resolve_to_gm_midi_numbers() {
    let dir = unique_tmpdir("drum-keys");
    let input = dir.join("song.codetta");
    let output = dir.join("song.mid");
    write_song(
        &input,
        &song_with(json!([drum_track(
            "drums",
            json!([
                { "t": 0.0, "pitch": "kick",  "dur": 0.25, "vel": 110 },
                { "t": 0.5, "pitch": "snare", "dur": 0.25, "vel": 110 },
            ]),
        )])),
    );

    let payload = run_export_expect_success(&input, &output);
    assert_eq!(payload["track_count"], 1);

    let bytes = std::fs::read(&output).unwrap();
    let smf = midly::Smf::parse(&bytes).unwrap();
    // MTrk 1 = drum (ch10)
    use midly::{MidiMessage, TrackEventKind};
    let keys: Vec<u8> = smf.tracks[1]
        .iter()
        .filter_map(|ev| match ev.kind {
            TrackEventKind::Midi {
                message: MidiMessage::NoteOn { key, .. },
                ..
            } => Some(key.as_int()),
            _ => None,
        })
        .collect();
    assert_eq!(keys, vec![36, 38]); // kick=36, snare=38

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn cli_sidecar_mode_writes_separate_meta_json() {
    let dir = unique_tmpdir("sidecar");
    let input = dir.join("song.codetta");
    let output = dir.join("song.mid");
    let sidecar = dir.join("song.codetta.meta.json");
    let mut song = song_with(json!([melodic_track(
        "lead",
        81,
        json!([{ "t": 0.0, "pitch": 60, "dur": 1.0, "vel": 100 }]),
    )]));
    song["metadata"]["master_gain"] = json!(1.5);
    write_song(&input, &song);

    let assert = Command::cargo_bin("codetta")
        .unwrap()
        .args([
            "export-midi",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            "--extensions",
            "sidecar",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let payload: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(payload["text_meta_written"], false);
    assert!(
        payload["sidecar"]
            .as_str()
            .unwrap()
            .ends_with("song.codetta.meta.json"),
        "sidecar path field present: {}",
        payload["sidecar"]
    );
    assert!(sidecar.exists(), "sidecar JSON must be written");

    let sidecar_json: Value = serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
    assert_eq!(sidecar_json["codetta"]["version"], "0.2");
    let mg = sidecar_json["codetta"]["metadata"]["master_gain"]
        .as_f64()
        .unwrap();
    assert!((mg - 1.5).abs() < 1e-3);

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    let _ = std::fs::remove_file(&sidecar);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn cli_implicit_migrate_handles_legacy_0_1_input() {
    // 0.1 schema (内蔵 synth saw_lead) を渡しても implicit migrate で 0.2 化して export できる
    let dir = unique_tmpdir("legacy");
    let input = dir.join("legacy.codetta");
    let output = dir.join("legacy.mid");
    let song = json!({
        "version": "0.1",
        "metadata": { "name": "legacy", "bpm": 120, "time_signature": [4, 4] },
        "tracks": [{
            "id": "lead",
            "name": "lead",
            "instrument": { "type": "saw_lead", "params": { "attack": 0.02 } },
            "volume": 0.8,
            "pan": 0.0,
            "notes": [
                { "t": 0.0, "pitch": 60, "dur": 1.0, "vel": 100 }
            ]
        }]
    });
    write_song(&input, &song);

    let payload = run_export_expect_success(&input, &output);
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["text_meta_written"], true);
    let migrate = &payload["implicit_migrate"];
    assert_eq!(migrate["from_version"], "0.1");
    assert_eq!(migrate["to_version"], "0.2");
    assert_eq!(migrate["tracks_migrated"], 1);
    assert_eq!(migrate["instrument_mapping"][0]["preset"], 81); // saw_lead → 81

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    let _ = std::fs::remove_dir(&dir);
}
