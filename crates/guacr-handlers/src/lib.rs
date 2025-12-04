// guacr-handlers: Protocol handler trait and registry for remote desktop protocols
//
// This crate defines the ProtocolHandler trait that all protocol implementations
// (SSH, RDP, VNC, Database, RBI) must implement, along with the registry for
// managing and dispatching to handlers.
//
// Also provides shared security, recording, and connection infrastructure used by all handlers.

mod connection;
mod error;
mod events;
mod handler;
mod integration;
mod multi_channel;
mod pipe;
mod recording;
mod registry;
mod security;

#[cfg(test)]
mod mock;

pub use connection::{
    connect_tcp_with_timeout, spawn_keepalive_task, ConnectionOptions, KeepAliveManager,
    DEFAULT_CONNECTION_TIMEOUT_SECS, DEFAULT_KEEPALIVE_INTERVAL_SECS,
};
pub use error::{HandlerError, Result};
pub use events::{
    connect_with_event_adapter, EventBasedHandler, EventCallback, HandlerEvent, InstructionSender,
};
pub use handler::{HandlerStats, HealthStatus, ProtocolHandler};
pub use integration::handle_guacd_with_handlers;
pub use multi_channel::{SimpleMultiChannelSender, WebRTCDataChannel};
pub use pipe::{
    // Pipe instruction formatting
    format_ack_instruction,
    format_end_instruction,
    format_pipe_blob,
    format_pipe_blob_raw,
    format_pipe_instruction,
    // Pipe instruction parsing
    parse_blob_instruction,
    parse_end_instruction,
    parse_pipe_instruction,
    pipe_blob_bytes,
    pipe_end_bytes,
    pipe_open_bytes,
    // Pipe stream types
    ParsedBlobInstruction,
    ParsedPipeInstruction,
    PipeStream,
    PipeStreamManager,
    // Pipe stream constants
    PIPE_AUTOFLUSH,
    PIPE_INTERPRET_OUTPUT,
    PIPE_NAME_STDIN,
    PIPE_NAME_STDOUT,
    PIPE_RAW,
    PIPE_STREAM_BASE,
    PIPE_STREAM_STDIN,
    PIPE_STREAM_STDOUT,
};
pub use recording::{
    GuacamoleSesRecorder, MultiFormatRecorder, RecordingConfig, RecordingDirection, RecordingError,
    RecordingFormat, SessionRecorder,
};
pub use registry::ProtocolHandlerRegistry;
pub use security::{
    // Database security
    check_sql_query_allowed,
    classify_sql_query,
    // Base security
    is_keyboard_event_allowed_readonly,
    is_mouse_event_allowed_readonly,
    is_mysql_export_query,
    is_mysql_import_query,
    is_postgres_copy_in,
    is_postgres_copy_out,
    DatabaseSecuritySettings,
    HandlerSecuritySettings,
    QueryType,
    // RBI security
    RbiSecuritySettings,
    ReadOnlyBehavior,
    // SFTP security
    SftpOperation,
    SftpSecuritySettings,
    CLIPBOARD_DEFAULT_SIZE,
    CLIPBOARD_MAX_SIZE,
    CLIPBOARD_MIN_SIZE,
};

#[cfg(test)]
pub use mock::MockProtocolHandler;
