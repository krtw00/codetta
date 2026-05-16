use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Song {
    pub version: String,
    pub metadata: Metadata,
    #[serde(default)]
    pub tracks: Vec<Track>,
}

impl Song {
    pub fn new(name: impl Into<String>, bpm: u32, key: Option<String>) -> Self {
        Self {
            version: crate::SCHEMA_VERSION.to_string(),
            metadata: Metadata {
                name: name.into(),
                bpm,
                key,
                time_signature: default_time_signature(),
                master_gain: default_master_gain(),
                created_at: None,
                tags: Vec::new(),
            },
            tracks: Vec::new(),
        }
    }

    /// 末尾ノートまでの長さ (ビート単位)。 ノート 0 個なら 0.0。
    pub fn duration_beats(&self) -> f32 {
        self.tracks
            .iter()
            .flat_map(|t| t.notes.iter())
            .map(|n| n.t + n.dur)
            .fold(0.0_f32, f32::max)
    }

    /// 秒換算 (`metadata.bpm` を参照)。
    pub fn duration_sec(&self) -> f32 {
        if self.metadata.bpm == 0 {
            return 0.0;
        }
        self.duration_beats() * 60.0 / self.metadata.bpm as f32
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Metadata {
    pub name: String,
    pub bpm: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default = "default_time_signature")]
    pub time_signature: [u32; 2],
    #[serde(default = "default_master_gain")]
    pub master_gain: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

fn default_time_signature() -> [u32; 2] {
    [4, 4]
}

fn default_master_gain() -> f32 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub id: String,
    pub name: String,
    pub instrument: Instrument,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fx: Vec<Effect>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
}

fn default_volume() -> f32 {
    0.8
}

/// Instrument は `{ "type": "...", "params": { ... } }`。
/// `params` の中身は楽器 type ごとに異なる。 Phase 0 では JSON のまま保持し、
/// 合成エンジン側で type を見てパースする。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Instrument {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Value>,
}

impl Instrument {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            params: Map::new(),
        }
    }
}

/// Effect は `{ "type": "...", <params...> }` (params が flat に並ぶ)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Effect {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(flatten)]
    pub params: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Note {
    pub t: f32,
    pub pitch: Pitch,
    pub dur: f32,
    #[serde(default = "default_velocity")]
    pub vel: u8,
}

fn default_velocity() -> u8 {
    100
}

/// Pitch はノート名 (`"C4"`, `"Bb3"`)、 MIDI 番号 (`60`)、
/// あるいは drum_kit 用のキー名 (`"kick"`) のいずれかを取る。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Pitch {
    Midi(u8),
    Name(String),
}

impl Pitch {
    /// MIDI 番号として解釈する (ノート名は変換、 drum 名はエラー)。
    pub fn as_midi(&self) -> Result<u8, PitchParseError> {
        match self {
            Pitch::Midi(n) => Ok(*n),
            Pitch::Name(s) => parse_note_name(s),
        }
    }

    /// drum キーとして解釈する (文字列のときのみ Ok)。
    pub fn as_drum_key(&self) -> Result<&str, PitchParseError> {
        match self {
            Pitch::Name(s) => Ok(s.as_str()),
            Pitch::Midi(_) => Err(PitchParseError::ExpectedDrumKey),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PitchParseError {
    Empty,
    UnknownNote(String),
    OctaveOutOfRange(i32),
    ExpectedDrumKey,
}

impl std::fmt::Display for PitchParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "empty pitch"),
            Self::UnknownNote(s) => write!(f, "unknown note name: {s:?}"),
            Self::OctaveOutOfRange(n) => write!(f, "octave out of MIDI range (-1..=9): {n}"),
            Self::ExpectedDrumKey => write!(f, "expected drum key string, got MIDI number"),
        }
    }
}

/// "C4" / "C#4" / "Db4" / "Bb3" 等を MIDI 番号に変換する (C4 = 60)。
pub fn parse_note_name(s: &str) -> Result<u8, PitchParseError> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return Err(PitchParseError::Empty);
    }

    // 1 文字目: A-G
    let semitone_base = match bytes[0].to_ascii_uppercase() {
        b'C' => 0,
        b'D' => 2,
        b'E' => 4,
        b'F' => 5,
        b'G' => 7,
        b'A' => 9,
        b'B' => 11,
        _ => return Err(PitchParseError::UnknownNote(s.to_string())),
    };

    // 2 文字目: optional '#' or 'b'
    let (accidental, rest) = match bytes.get(1) {
        Some(b'#') => (1_i32, &s[2..]),
        Some(b'b') => (-1_i32, &s[2..]),
        _ => (0_i32, &s[1..]),
    };

    // 残りはオクターブ (符号付き整数)。 例: "-1", "0", "10"
    let octave: i32 = rest
        .parse()
        .map_err(|_| PitchParseError::UnknownNote(s.to_string()))?;

    // MIDI: C-1 = 0, C0 = 12, C4 = 60
    let midi = (octave + 1) * 12 + semitone_base + accidental;
    if !(0..=127).contains(&midi) {
        return Err(PitchParseError::OctaveOutOfRange(octave));
    }
    Ok(midi as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_name_basics() {
        assert_eq!(parse_note_name("C4").unwrap(), 60);
        assert_eq!(parse_note_name("A4").unwrap(), 69);
        assert_eq!(parse_note_name("C-1").unwrap(), 0);
        assert_eq!(parse_note_name("G9").unwrap(), 127);
    }

    #[test]
    fn note_name_accidentals() {
        assert_eq!(parse_note_name("C#4").unwrap(), 61);
        assert_eq!(parse_note_name("Db4").unwrap(), 61);
        assert_eq!(parse_note_name("Bb3").unwrap(), 58);
    }

    #[test]
    fn note_name_errors() {
        assert!(matches!(parse_note_name(""), Err(PitchParseError::Empty)));
        assert!(matches!(
            parse_note_name("H4"),
            Err(PitchParseError::UnknownNote(_))
        ));
        assert!(matches!(
            parse_note_name("Cx"),
            Err(PitchParseError::UnknownNote(_))
        ));
        assert!(matches!(
            parse_note_name("C99"),
            Err(PitchParseError::OctaveOutOfRange(_))
        ));
    }

    #[test]
    fn pitch_serde_roundtrip_string() {
        let p = Pitch::Name("C4".into());
        let j = serde_json::to_string(&p).unwrap();
        assert_eq!(j, r#""C4""#);
        let back: Pitch = serde_json::from_str(&j).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn pitch_serde_roundtrip_midi() {
        let p = Pitch::Midi(60);
        let j = serde_json::to_string(&p).unwrap();
        assert_eq!(j, "60");
        let back: Pitch = serde_json::from_str(&j).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn duration_beats_picks_max_end() {
        let mut s = Song::new("t", 120, None);
        s.tracks.push(Track {
            id: "lead".into(),
            name: "lead".into(),
            instrument: Instrument::new("sin"),
            volume: 0.8,
            pan: 0.0,
            mute: false,
            solo: false,
            fx: vec![],
            notes: vec![
                Note {
                    t: 0.0,
                    pitch: Pitch::Name("C4".into()),
                    dur: 1.5,
                    vel: 100,
                },
                Note {
                    t: 2.0,
                    pitch: Pitch::Name("E4".into()),
                    dur: 1.0,
                    vel: 100,
                },
            ],
        });
        assert!((s.duration_beats() - 3.0).abs() < f32::EPSILON);
        // 120 BPM で 3 ビート = 1.5 秒
        assert!((s.duration_sec() - 1.5).abs() < 1e-6);
    }
}
