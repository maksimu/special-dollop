// guacr-handlers: Protocol handler trait and registry for remote desktop protocols
//
// This crate defines the ProtocolHandler trait that all protocol implementations
// (SSH, RDP, VNC, Database, RBI) must implement, along with the registry for
// managing and dispatching to handlers.
//
// Also provides shared security, recording, and connection infrastructure used by all handlers.
//
// Feature flags:
// - `guacd-compat`: Enable guacd wire protocol compatibility (select/args/connect/ready handshake)
// - `vsphere`: Enable vSphere REST API client for VM management and console access
// - `docker`: Enable Docker container management via bollard
// - `kubernetes`: Enable Kubernetes API client for pod management, exec, and log viewing

mod adaptive_quality;
mod connection;
mod cursor;
mod drag_detector;
mod error;
mod events;
mod handler;
mod host_key;
mod integration;
mod multi_channel;
mod pipe;
mod recording;
mod registry;
pub mod resource_browser;
mod security;
mod session;
mod sync_control;
mod throughput;

#[cfg(feature = "guacd-compat")]
mod handshake;

#[cfg(feature = "vsphere")]
mod vsphere;

#[cfg(feature = "docker")]
pub mod docker_handler;

#[cfg(feature = "kubernetes")]
pub mod kubernetes;

#[cfg(test)]
mod mock;

pub use adaptive_quality::AdaptiveQuality;
pub use connection::{
    connect_tcp_with_timeout, spawn_keepalive_task, ConnectionOptions, KeepAliveManager,
    DEFAULT_CONNECTION_TIMEOUT_SECS, DEFAULT_KEEPALIVE_INTERVAL_SECS,
};
pub use cursor::{send_cursor_instructions, CursorManager, StandardCursor};
pub use drag_detector::DragDetector;
pub use error::{send_error_and_abort, send_error_best_effort, HandlerError, Result};
pub use events::{
    connect_with_event_adapter, EventBasedHandler, EventCallback, HandlerEvent, InstructionSender,
};
#[cfg(feature = "guacd-compat")]
pub use handler::HandlerArg;
pub use handler::{HandlerStats, HealthStatus, ProtocolHandler};
pub use host_key::{calculate_fingerprint, HostKeyConfig, HostKeyResult, HostKeyVerifier};
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
pub use session::{send_bell, send_disconnect, send_name, send_ready};
pub use sync_control::SyncFlowControl;
pub use throughput::ThroughputTracker;

// guacd wire protocol compatibility exports
#[cfg(feature = "guacd-compat")]
pub use handshake::{
    get_protocol_arg_names, get_protocol_args, handle_guacd_connection,
    handle_guacd_connection_with_timeout, protocol_args, run_guacd_server, status, ArgDescriptor,
    ConnectResult, GuacdHandshake, GuacdServer, HandshakeError, SelectResult,
    DEFAULT_HANDSHAKE_TIMEOUT_SECS, GUACAMOLE_PROTOCOL_VERSION, GUACD_DEFAULT_PORT,
    MAX_INSTRUCTION_SIZE,
};

// vSphere REST API client exports
#[cfg(feature = "vsphere")]
pub use vsphere::{
    ConsoleTicket, DiskInfo, HostInfo, NicInfo, PowerState, VSphereClient, VmDetail, VmInfo,
};

// Docker container management exports
#[cfg(feature = "docker")]
pub use docker_handler::{
    detect_connection_mode, ContainerInfo, DockerClient, DockerExecReader, DockerExecWriter,
    DockerLogReader,
};

// Kubernetes API client exports
#[cfg(feature = "kubernetes")]
pub use kubernetes::{
    detect_auth_method, AuthMethod, ExecReader, ExecWriter, KubernetesClient, LogReader, PodInfo,
};

#[cfg(test)]
pub use mock::MockProtocolHandler;
