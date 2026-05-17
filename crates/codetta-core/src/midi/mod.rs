//! MIDI import (CDT-3) と export (CDT-4)。
//!
//! 設計: docs/design/08-midi.md

mod error;
mod export;
mod extensions;
mod import;

pub use error::MidiError;
pub use export::{
    export_song, MidiExportOptions, MidiExportOutcome, MidiExportWarning, DEFAULT_PPQ,
};
pub use extensions::ExtensionsMode;
pub use import::{
    import_midi, ExtensionsRecovered, MidiImportOptions, MidiImportOutcome, MidiImportWarning,
};
