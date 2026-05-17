//! `codetta import-midi` の integration test。
//!
//! CDT-3。 fixture .mid は midly で in-test 生成する (= binary を repo に持たない)。
//! ADR: docs/design/08-midi.md

use std::path::PathBuf;

use assert_cmd::Command;
use midly::{
    num::{u15, u24, u28, u4, u7},
    Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind,
};
use serde_json::Value;

fn unique_tmpdir(stem: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut p = std::env::temp_dir();
    p.push(format!(
        "codetta-midi-import-{stem}-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn ev(delta: u32, kind: TrackEventKind<'_>) -> TrackEvent<'_> {
    TrackEvent {
        delta: u28::new(delta),
        kind,
    }
}

/// melodic ch1 (program 81) + drum ch10 (kick + snare) の Type 1 SMF を生成。
fn build_basic_gm_mid() -> Vec<u8> {
    let ppq: u16 = 480;
    let meta = vec![
        ev(
            0,
            TrackEventKind::Meta(MetaMessage::Tempo(u24::new(500_000))),
        ),
        ev(
            0,
            TrackEventKind::Meta(MetaMessage::TimeSignature(4, 2, 24, 8)),
        ),
        ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ];

    let mel = vec![
        // CC7 volume = 100、 CC10 pan = 80 (= 右寄り)
        ev(
            0,
            TrackEventKind::Midi {
                channel: u4::new(0),
                message: MidiMessage::Controller {
                    controller: u7::new(7),
                    value: u7::new(100),
                },
            },
        ),
        ev(
            0,
            TrackEventKind::Midi {
                channel: u4::new(0),
                message: MidiMessage::Controller {
                    controller: u7::new(10),
                    value: u7::new(80),
                },
            },
        ),
        ev(
            0,
            TrackEventKind::Midi {
                channel: u4::new(0),
                message: MidiMessage::ProgramChange {
                    program: u7::new(81),
                },
            },
        ),
        // C4 quarter
        ev(
            0,
            TrackEventKind::Midi {
                channel: u4::new(0),
                message: MidiMessage::NoteOn {
                    key: u7::new(60),
                    vel: u7::new(100),
                },
            },
        ),
        ev(
            u32::from(ppq),
            TrackEventKind::Midi {
                channel: u4::new(0),
                message: MidiMessage::NoteOff {
                    key: u7::new(60),
                    vel: u7::new(0x40),
                },
            },
        ),
        ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ];

    let drum = vec![
        // kick at t=0, snare at t=1 beat
        ev(
            0,
            TrackEventKind::Midi {
                channel: u4::new(9),
                message: MidiMessage::NoteOn {
                    key: u7::new(36),
                    vel: u7::new(110),
                },
            },
        ),
        ev(
            u32::from(ppq / 4),
            TrackEventKind::Midi {
                channel: u4::new(9),
                message: MidiMessage::NoteOff {
                    key: u7::new(36),
                    vel: u7::new(0x40),
                },
            },
        ),
        ev(
            u32::from(ppq * 3 / 4),
            TrackEventKind::Midi {
                channel: u4::new(9),
                message: MidiMessage::NoteOn {
                    key: u7::new(38),
                    vel: u7::new(120),
                },
            },
        ),
        ev(
            u32::from(ppq / 4),
            TrackEventKind::Midi {
                channel: u4::new(9),
                message: MidiMessage::NoteOff {
                    key: u7::new(38),
                    vel: u7::new(0x40),
                },
            },
        ),
        ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ];

    let smf = Smf {
        header: Header::new(Format::Parallel, Timing::Metrical(u15::new(ppq))),
        tracks: vec![meta, mel, drum],
    };
    let mut buf = Vec::new();
    smf.write(&mut buf).unwrap();
    buf
}

fn run_import(input: &std::path::Path, output: &std::path::Path) -> (Value, Value) {
    let assert = Command::cargo_bin("codetta")
        .unwrap()
        .args([
            "import-midi",
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
fn cli_imports_basic_gm_song_with_channel_mapping() {
    let dir = unique_tmpdir("basic");
    let input = dir.join("song.mid");
    let output = dir.join("song.codetta");
    std::fs::write(&input, build_basic_gm_mid()).unwrap();

    let (payload, song) = run_import(&input, &output);

    assert_eq!(payload["ok"], true);
    assert_eq!(payload["source_format"], "Type 1");
    assert_eq!(payload["ppq"], 480);
    assert_eq!(payload["track_count"], 2);
    assert_eq!(payload["extensions_recovered"]["source"], "none");

    assert_eq!(song["version"], "0.2");
    assert_eq!(song["metadata"]["bpm"], 120);
    assert_eq!(
        song["metadata"]["time_signature"],
        serde_json::json!([4, 4])
    );

    let tracks = song["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 2);

    // ch1 melodic: program 81, volume CC7=100 → 100/127, pan CC10=80 → (80-64)/63 ≈ 0.254
    let mel = &tracks[0];
    assert_eq!(mel["id"], "channel-1");
    assert_eq!(mel["instrument"]["type"], "soundfont");
    assert_eq!(mel["instrument"]["params"]["preset"], 81);
    assert_eq!(mel["instrument"]["params"]["bank"], 0);
    let vol = mel["volume"].as_f64().unwrap();
    assert!((vol - 100.0 / 127.0).abs() < 1e-3, "volume={vol}");
    let pan = mel["pan"].as_f64().unwrap();
    assert!(pan > 0.0 && pan < 0.5, "pan should be right-ish: {pan}");
    let mel_notes = mel["notes"].as_array().unwrap();
    assert_eq!(mel_notes.len(), 1);
    assert_eq!(mel_notes[0]["pitch"], 60);

    // ch10 drum: bank 128, preset 0 (no program change), kick + snare at数値固定
    let drum = &tracks[1];
    assert_eq!(drum["id"], "drums");
    assert_eq!(drum["instrument"]["params"]["bank"], 128);
    assert_eq!(drum["instrument"]["params"]["preset"], 0);
    let drum_notes = drum["notes"].as_array().unwrap();
    assert_eq!(drum_notes.len(), 2);
    // 出現順 (t 昇順)
    assert_eq!(drum_notes[0]["pitch"], 36); // kick
    assert_eq!(drum_notes[1]["pitch"], 38); // snare
    assert!((drum_notes[1]["t"].as_f64().unwrap() - 1.0).abs() < 1e-3);

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn cli_refuses_to_overwrite_without_force() {
    let dir = unique_tmpdir("overwrite");
    let input = dir.join("song.mid");
    let output = dir.join("song.codetta");
    std::fs::write(&input, build_basic_gm_mid()).unwrap();
    std::fs::write(&output, b"placeholder").unwrap();

    let assert = Command::cargo_bin("codetta")
        .unwrap()
        .args([
            "import-midi",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let payload: Value = serde_json::from_str(stdout.trim()).expect("CLI stdout JSON");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["errors"][0]["code"], "FILE_EXISTS");
    assert_eq!(std::fs::read(&output).unwrap(), b"placeholder");

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    let _ = std::fs::remove_dir(&dir);
}
