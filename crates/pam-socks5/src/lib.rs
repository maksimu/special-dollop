//! SOCKS5 protocol handshake and payload parsing for keeper-pam tunneling.
//!
//! This crate provides:
//! - `Socks5Handshake<S>`: Generic SOCKS5 handshake handler over any async stream
//! - Payload encoding/decoding for OpenConnection messages
//! - All SOCKS5 protocol constants

mod handshake;
mod payload;

pub use handshake::{
    Socks5Address, Socks5Command, Socks5Handshake, SOCKS5_ADDR_TYPE_IPV4, SOCKS5_ATYP_DOMAIN,
    SOCKS5_ATYP_IPV6, SOCKS5_AUTH_FAILED, SOCKS5_AUTH_METHOD_NONE, SOCKS5_CMD_CONNECT, SOCKS5_FAIL,
    SOCKS5_SUCCESS_RESPONSE, SOCKS5_VERSION,
};
pub use payload::{decode_open_connection_payload, encode_open_connection_payload, Socks5Target};
