// guacr-guacd: guacd wire protocol compatibility layer
//
// Implements the Guacamole handshake protocol (select/args/connect/ready)
// to allow guacr to act as a drop-in replacement for guacd.
//
// Previously part of guacr-handlers (behind guacd-compat feature flag).
// Now a standalone crate with clean ownership of the guacd protocol layer.
//
// Protocol flow:
// 1. Client -> Server: select,<protocol>;
// 2. Server -> Client: args,<arg1>,<arg2>,...;
// 3. Client -> Server: connect,<val1>,<val2>,...;
// 4. Server -> Client: ready,<connection-id>;
// 5. Interactive phase begins

mod args;
pub mod client;
mod server;

pub use args::{get_protocol_arg_names, get_protocol_args, protocol_args, ArgDescriptor};
pub use server::{
    handle_guacd_connection, handle_guacd_connection_with_timeout, run_guacd_server, status,
    ConnectResult, GuacdHandshake, GuacdServer, HandshakeError, SelectResult,
    DEFAULT_HANDSHAKE_TIMEOUT_SECS, GUACAMOLE_PROTOCOL_VERSION, GUACD_DEFAULT_PORT,
    MAX_INSTRUCTION_SIZE,
};
