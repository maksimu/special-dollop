//! Oracle TNS Protocol Support
//!
//! This module implements the Oracle TNS (Transparent Network Substrate)
//! protocol for proxying Oracle Database connections.
//!
//! # Protocol Overview
//!
//! TNS is Oracle's proprietary network protocol for database communication.
//! All multi-byte fields are big-endian.
//!
//! # Module Structure
//!
//! - `constants` - Protocol constants (packet types, versions, flags)
//! - `packets` - Packet structures (TnsHeader, etc.)
//! - `parser` - Packet parsing and serialization functions
//! - `auth` - O5LOGON authentication support

pub mod auth;
pub mod constants;
pub mod packets;
pub mod parser;

// Re-export commonly used types
pub use constants::*;
pub use packets::{
    AcceptPacket, ConnectPacket, DataPacket, RefusePacket, TnsHeader, ACCEPT_PACKET_FIXED_SIZE,
    CONNECT_PACKET_FIXED_SIZE, CONNECT_PACKET_MIN_SIZE, REFUSE_PACKET_FIXED_SIZE,
};
pub use parser::{
    // Header functions
    build_packet,
    modify_connect_packet_address,
    modify_connect_packet_service_name,
    modify_connect_packet_user,
    modify_connect_string_user,
    packet_complete,
    parse_accept,
    // Packet parsing/serialization
    parse_connect,
    parse_connect_string,
    parse_data,
    parse_header,
    parse_refuse,
    read_packet,
    serialize_accept,
    serialize_connect,
    serialize_data,
    serialize_header,
    serialize_refuse,
    strip_connect_packet_cid_and_connid,
    strip_connect_packet_nsd_preamble,
    update_packet_length,
    // Connect string parsing
    ConnectStringParts,
};

pub use auth::{
    build_auth_error,
    build_auth_response_packet,
    // Client sesskey preservation (when client provides credentials)
    build_auth_response_packet_preserve_sesskey,
    // Proxy auth mode
    build_proxy_credential_packet,
    extract_challenge_data,
    extract_client_auth_password,
    extract_client_auth_sesskey,
    extract_full_challenge_data,
    extract_pbkdf2_params,
    is_auth_failure,
    is_auth_packet,
    is_auth_response_packet,
    is_auth_success,
    modify_auth_username,
    parse_auth_challenge,
    parse_auth_request,
    replace_auth_username,
    AuthChallenge,
    AuthRequest,
    AuthResponseData,
    AuthResult,
    ChallengeData,
    // O5LOGON crypto
    O5LogonAuth,
    O5LogonState,
    // Oracle 12c+ PBKDF2 support
    Pbkdf2Params,
};
