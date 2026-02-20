// guacr-recording: Session recording for Guacamole protocol handlers
//
// Provides unified recording infrastructure for all protocol handlers:
// - Guacamole .ses format (raw protocol recording for guacenc/guaclog)
// - Asciicast v2 format (terminal sessions for asciinema player)
// - Typescript format (legacy text logging with timing)
// - Pluggable transports (files, channels, etc.)
//
// This crate is a leaf dependency with no guacr crate dependencies, allowing
// all handler crates to depend on it without circular dependencies.

mod asciicast;
mod config;
mod helpers;
mod multi;
mod ses;
mod transport;

pub use asciicast::{AsciicastHeader, AsciicastRecorder, EventType};
pub use config::{find_unique_path, get_param, parse_bool, RecordingConfig, RecordingFormat};
pub use helpers::{extract_opcode, inject_timestamp, is_drawing_instruction};
pub use multi::MultiFormatRecorder;
pub use ses::{GuacamoleSesRecorder, RecordingDirection, RecordingError, SessionRecorder};
pub use transport::{
    ChannelRecordingTransport, FileRecordingTransport, MultiTransportRecorder, RecordingTransport,
};
