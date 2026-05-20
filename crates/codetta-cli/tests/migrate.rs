//! `codetta migrate` の round-trip integration test。
//!
//! step 6 / step 7 で追加。 LUT 全 entry の round-trip と、
//! fallback warning + 既に 0.2 の入力に対する `MIGRATE_NOT_NEEDED` を確認する。

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn unique_tmp(stem: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut p = std::env::temp_dir();
    p.push(format!(
        "codetta-mig-{stem}-{nanos}-{}.codetta",
        std::process::id()
    ));
    p
}

fn run_migrate(input: &Path, output: &Path) -> (Value, Value) {
    let assert = Command::cargo_bin("codetta")
        .unwrap()
        .args([
            "migrate",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let payload: Value = serde_json::from_str(stdout.trim()).expect("CLI stdout JSON");
    let written = std::fs::read(output).expect("output written");
    let song: Value = serde_json::from_slice(&written).expect("output JSON");
    (payload, song)
}

#[test]
fn migrate_all_lut_entries_round_trip() {
    let input = fixture("migrate-0.1-all-instruments.codetta");
    let output = unique_tmp("all");
    let (payload, song) = run_migrate(&input, &output);

    assert_eq!(payload["ok"], true);
    assert_eq!(payload["from_version"], "0.1");
    assert_eq!(payload["to_version"], "0.2");
    assert_eq!(payload["tracks_migrated"], 8);
    assert!(
        payload.get("warnings").is_none(),
        "no warnings expected for LUT-only input"
    );
    assert_eq!(song["version"], "0.2");

    let expected = [
        ("sin-track", "sin", 38_u64, 0_u64),
        ("saw-track", "saw", 81, 0),
        ("saw-lead-track", "saw_lead", 81, 0),
        ("square-track", "square", 80, 0),
        ("square-bass-track", "square_bass", 80, 0),
        ("triangle-track", "triangle", 73, 0),
        ("saw-pad-track", "saw_pad", 88, 0),
        ("drum-track", "drum_kit", 0, 128),
    ];

    let mapping = payload["instrument_mapping"].as_array().unwrap();
    assert_eq!(mapping.len(), expected.len());
    for (track_id, from_kind, preset, bank) in expected {
        let m = mapping
            .iter()
            .find(|m| m["track_id"] == track_id)
            .unwrap_or_else(|| panic!("mapping missing {track_id}"));
        assert_eq!(m["from_kind"], from_kind);
        assert_eq!(m["to_kind"], "soundfont");
        assert_eq!(m["preset"], preset);
        assert_eq!(m["bank"], bank);
        assert_eq!(m["fallback"], false);

        let track = song["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == track_id)
            .unwrap();
        assert_eq!(track["instrument"]["type"], "soundfont");
        assert_eq!(track["instrument"]["params"]["preset"], preset);
        assert_eq!(track["instrument"]["params"]["bank"], bank);
        assert_eq!(track["instrument"]["params"]["file"], "GeneralUser-GS.sf2");
        // 旧 params (attack / pulse_width / detune_cents / kit) は破棄
        for legacy in [
            "attack",
            "decay",
            "sustain",
            "release",
            "pulse_width",
            "detune_cents",
            "kit",
        ] {
            assert!(
                track["instrument"]["params"].get(legacy).is_none(),
                "legacy param {legacy} should be dropped on {track_id}"
            );
        }
    }
    let _ = std::fs::remove_file(&output);
}

#[test]
fn migrate_unknown_kind_emits_warning_and_uses_preset_zero() {
    let input = fixture("migrate-0.1-with-unknown.codetta");
    let output = unique_tmp("unknown");
    let (payload, song) = run_migrate(&input, &output);

    assert_eq!(payload["ok"], true);
    assert_eq!(payload["tracks_migrated"], 2);

    let warnings = payload["warnings"].as_array().expect("warnings array");
    assert_eq!(warnings.len(), 1);
    let w = &warnings[0];
    assert_eq!(w["code"], "MIGRATE_UNKNOWN_INSTRUMENT");
    assert_eq!(w["track_id"], "weird");
    let msg = w["message"].as_str().unwrap();
    assert!(
        msg.contains("frobnicator"),
        "warning message should mention the unknown kind: {msg}"
    );
    assert!(
        msg.contains("preset 0"),
        "warning message should mention preset 0 fallback: {msg}"
    );

    let mapping = payload["instrument_mapping"].as_array().unwrap();
    let weird = mapping.iter().find(|m| m["track_id"] == "weird").unwrap();
    assert_eq!(weird["preset"], 0);
    assert_eq!(weird["bank"], 0);
    assert_eq!(weird["fallback"], true);

    let known = mapping.iter().find(|m| m["track_id"] == "known").unwrap();
    assert_eq!(known["preset"], 38);
    assert_eq!(known["fallback"], false);

    let weird_track = song["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "weird")
        .unwrap();
    assert_eq!(weird_track["instrument"]["type"], "soundfont");
    assert_eq!(weird_track["instrument"]["params"]["preset"], 0);
    assert_eq!(weird_track["instrument"]["params"]["bank"], 0);
    let _ = std::fs::remove_file(&output);
}

#[test]
fn migrate_on_already_v02_returns_not_needed_error() {
    let input = fixture("migrate-0.2-already.codetta");
    let output = unique_tmp("already");
    let assert = Command::cargo_bin("codetta")
        .unwrap()
        .args([
            "migrate",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let payload: Value = serde_json::from_str(stdout.trim()).expect("CLI stdout JSON");
    assert_eq!(payload["ok"], false);
    let errors = payload["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["code"], "MIGRATE_NOT_NEEDED");
    // 出力ファイルは書き込まれていない
    assert!(
        !output.exists(),
        "output should not be written when migrate is not needed"
    );
}
