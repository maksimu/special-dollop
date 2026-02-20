// Re-export all recording types from guacr-recording.
//
// This module previously contained the recording implementation. It has been
// moved to the dedicated `guacr-recording` leaf crate to eliminate duplication
// and circular dependencies. All public types are re-exported here for backward
// compatibility.

pub use guacr_recording::*;
