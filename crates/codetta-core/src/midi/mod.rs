//! MIDI import (CDT-3)。 export は CDT-4 で追加予定。
//!
//! 設計: docs/design/08-midi.md

mod error;
mod extensions;
mod import;

pub use error::MidiError;
pub use extensions::ExtensionsMode;
pub use import::{
    import_midi, ExtensionsRecovered, MidiImportOptions, MidiImportOutcome, MidiImportWarning,
};
