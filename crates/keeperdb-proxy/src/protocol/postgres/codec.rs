//! PostgreSQL message codec (read/write)
//!
//! This module provides functions for reading and writing PostgreSQL wire protocol messages.
//! Reference: <https://www.postgresql.org/docs/current/protocol-message-formats.html>

use std::collections::HashMap;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{ProxyError, Result};

use super::constants::*;
use super::messages::*;

// ============================================================================
// Constants
// ============================================================================

/// Maximum message size (100MB default, protocol allows up to 1GB)
pub const MAX_MESSAGE_SIZE: u32 = 100 * 1024 * 1024;

/// Minimum message length (just the 4-byte length field)
pub const MIN_MESSAGE_LENGTH: u32 = 4;

// ============================================================================
// Low-Level Read Helpers
// ============================================================================

/// Read a single byte.
async fn read_u8<R: AsyncRead + Unpin>(reader: &mut R) -> Result<u8> {
    let mut buf = [0u8; 1];
    reader.read_exact(&mut buf).await?;
    Ok(buf[0])
}

/// Read a u16 in big-endian format.
#[allow(dead_code)]
async fn read_u16_be<R: AsyncRead + Unpin>(reader: &mut R) -> Result<u16> {
    let mut buf = [0u8; 2];
    reader.read_exact(&mut buf).await?;
    Ok(u16::from_be_bytes(buf))
}

/// Read a u32 in big-endian format.
async fn read_u32_be<R: AsyncRead + Unpin>(reader: &mut R) -> Result<u32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf).await?;
    Ok(u32::from_be_bytes(buf))
}

/// Read an i16 in big-endian format.
#[allow(dead_code)]
async fn read_i16_be<R: AsyncRead + Unpin>(reader: &mut R) -> Result<i16> {
    let mut buf = [0u8; 2];
    reader.read_exact(&mut buf).await?;
    Ok(i16::from_be_bytes(buf))
}

/// Read an i32 in big-endian format.
#[allow(dead_code)]
async fn read_i32_be<R: AsyncRead + Unpin>(reader: &mut R) -> Result<i32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf).await?;
    Ok(i32::from_be_bytes(buf))
}

/// Read a null-terminated string from a buffer at the given offset.
/// Returns the string and the number of bytes consumed (including null).
fn read_cstring_from_buf(buf: &[u8], offset: usize) -> Result<(String, usize)> {
    let start = offset;
    let end = buf[start..]
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| ProxyError::Protocol("Missing null terminator in string".into()))?;

    let s = std::str::from_utf8(&buf[start..start + end])
        .map_err(|_| ProxyError::Protocol("Invalid UTF-8 in string".into()))?;

    Ok((s.to_string(), end + 1)) // +1 for null terminator
}

/// Read a null-terminated string directly from the reader.
#[allow(dead_code)]
async fn read_cstring<R: AsyncRead + Unpin>(reader: &mut R) -> Result<String> {
    let mut bytes = Vec::new();
    loop {
        let b = read_u8(reader).await?;
        if b == 0 {
            break;
        }
        bytes.push(b);
    }

    String::from_utf8(bytes).map_err(|_| ProxyError::Protocol("Invalid UTF-8 in string".into()))
}

// ============================================================================
// Low-Level Write Helpers
// ============================================================================

/// Write a u16 in big-endian format.
#[allow(dead_code)]
async fn write_u16_be<W: AsyncWrite + Unpin>(writer: &mut W, value: u16) -> Result<()> {
    writer.write_all(&value.to_be_bytes()).await?;
    Ok(())
}

/// Write a u32 in big-endian format.
async fn write_u32_be<W: AsyncWrite + Unpin>(writer: &mut W, value: u32) -> Result<()> {
    writer.write_all(&value.to_be_bytes()).await?;
    Ok(())
}

/// Write an i16 in big-endian format.
#[allow(dead_code)]
async fn write_i16_be<W: AsyncWrite + Unpin>(writer: &mut W, value: i16) -> Result<()> {
    writer.write_all(&value.to_be_bytes()).await?;
    Ok(())
}

/// Write an i32 in big-endian format.
#[allow(dead_code)]
async fn write_i32_be<W: AsyncWrite + Unpin>(writer: &mut W, value: i32) -> Result<()> {
    writer.write_all(&value.to_be_bytes()).await?;
    Ok(())
}

/// Write a null-terminated string.
#[allow(dead_code)]
async fn write_cstring<W: AsyncWrite + Unpin>(writer: &mut W, s: &str) -> Result<()> {
    writer.write_all(s.as_bytes()).await?;
    writer.write_all(&[0]).await?;
    Ok(())
}

// ============================================================================
// Message Reading
// ============================================================================

/// Read a startup message from a new connection.
///
/// Startup messages have a different format: Length (4) + Version/Code (4) + Data
/// No type byte prefix.
///
/// Returns the startup message or information about what was received
/// (could be SSLRequest, CancelRequest, or StartupMessage).
pub async fn read_startup_message<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<StartupMessageType> {
    // Read length
    let length = read_u32_be(reader).await?;

    // Validate length
    if length < 8 {
        return Err(ProxyError::Protocol(format!(
            "Startup message too short: {} bytes",
            length
        )));
    }
    if length > MAX_MESSAGE_SIZE {
        return Err(ProxyError::Protocol(format!(
            "Startup message too large: {} bytes",
            length
        )));
    }

    // Read version/code
    let code = read_u32_be(reader).await?;

    // Check for special request types
    if code == SSL_REQUEST_CODE {
        // SSLRequest - no more data
        return Ok(StartupMessageType::SSLRequest);
    }

    if code == CANCEL_REQUEST_CODE {
        // CancelRequest - read process_id and secret_key
        let process_id = read_u32_be(reader).await?;
        let secret_key = read_u32_be(reader).await?;
        return Ok(StartupMessageType::CancelRequest(CancelRequest {
            process_id,
            secret_key,
        }));
    }

    // Regular startup message - code is protocol version
    if code != PROTOCOL_VERSION_3_0 {
        return Err(ProxyError::Protocol(format!(
            "Unsupported protocol version: {} (expected {})",
            code, PROTOCOL_VERSION_3_0
        )));
    }

    // Read remaining data (parameters)
    let remaining = (length - 8) as usize;
    let mut buf = vec![0u8; remaining];
    reader.read_exact(&mut buf).await?;

    // Parse parameters (null-terminated key-value pairs, ending with empty string)
    let mut parameters = HashMap::new();
    let mut offset = 0;

    while offset < buf.len() {
        // Read key
        let (key, key_len) = read_cstring_from_buf(&buf, offset)?;
        offset += key_len;

        if key.is_empty() {
            // Empty key signals end of parameters
            break;
        }

        // Read value
        if offset >= buf.len() {
            return Err(ProxyError::Protocol("Missing value for parameter".into()));
        }
        let (value, value_len) = read_cstring_from_buf(&buf, offset)?;
        offset += value_len;

        parameters.insert(key, value);
    }

    Ok(StartupMessageType::Startup(StartupMessage {
        protocol_version: code,
        parameters,
    }))
}

/// Type of startup message received.
#[derive(Debug)]
pub enum StartupMessageType {
    /// Regular startup message with connection parameters
    Startup(StartupMessage),
    /// SSL upgrade request
    SSLRequest,
    /// Query cancellation request
    CancelRequest(CancelRequest),
}

/// Read a typed message (type byte + length + payload).
///
/// Returns the message type byte and the payload (without the length).
pub async fn read_message<R: AsyncRead + Unpin>(reader: &mut R) -> Result<(u8, Vec<u8>)> {
    // Read type byte
    let msg_type = read_u8(reader).await?;

    // Read length (includes itself, 4 bytes)
    let length = read_u32_be(reader).await?;

    // Validate length
    if length < MIN_MESSAGE_LENGTH {
        return Err(ProxyError::Protocol(format!(
            "Invalid message length: {}",
            length
        )));
    }
    if length > MAX_MESSAGE_SIZE {
        return Err(ProxyError::Protocol(format!(
            "Message too large: {} bytes (max: {})",
            length, MAX_MESSAGE_SIZE
        )));
    }

    // Read payload (length - 4 for the length field itself)
    let payload_len = (length - 4) as usize;
    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        reader.read_exact(&mut payload).await?;
    }

    Ok((msg_type, payload))
}

// ============================================================================
// Message Writing
// ============================================================================

/// Write a typed message (type byte + length + payload).
pub async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    msg_type: u8,
    payload: &[u8],
) -> Result<()> {
    // Write type byte
    writer.write_all(&[msg_type]).await?;

    // Write length (payload + 4 for length field)
    let length = (payload.len() + 4) as u32;
    write_u32_be(writer, length).await?;

    // Write payload
    if !payload.is_empty() {
        writer.write_all(payload).await?;
    }

    writer.flush().await?;
    Ok(())
}

/// Write a startup message to the server.
pub async fn write_startup_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    msg: &StartupMessage,
) -> Result<()> {
    // Build the message content first to calculate length
    let mut content = Vec::new();

    // Protocol version
    content.extend_from_slice(&msg.protocol_version.to_be_bytes());

    // Parameters (key\0value\0 pairs)
    for (key, value) in &msg.parameters {
        content.extend_from_slice(key.as_bytes());
        content.push(0);
        content.extend_from_slice(value.as_bytes());
        content.push(0);
    }

    // Terminating null
    content.push(0);

    // Length includes itself (4 bytes)
    let length = (content.len() + 4) as u32;

    // Write length
    write_u32_be(writer, length).await?;

    // Write content
    writer.write_all(&content).await?;
    writer.flush().await?;

    Ok(())
}

/// Write an SSL request.
pub async fn write_ssl_request<W: AsyncWrite + Unpin>(writer: &mut W) -> Result<()> {
    // Length (8) + SSL request code
    write_u32_be(writer, SSLRequest::LENGTH).await?;
    write_u32_be(writer, SSLRequest::CODE).await?;
    writer.flush().await?;
    Ok(())
}

// ============================================================================
// Message Parsing (from payload)
// ============================================================================

/// Parse an authentication message from payload.
pub fn parse_authentication(payload: &[u8]) -> Result<AuthenticationMessage> {
    if payload.len() < 4 {
        return Err(ProxyError::Protocol(
            "Authentication message too short".into(),
        ));
    }

    let auth_type = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);

    match auth_type {
        AUTH_OK => Ok(AuthenticationMessage::Ok),

        AUTH_KERBEROS_V5 => Ok(AuthenticationMessage::KerberosV5),

        AUTH_CLEARTEXT_PASSWORD => Ok(AuthenticationMessage::CleartextPassword),

        AUTH_MD5_PASSWORD => {
            if payload.len() < 8 {
                return Err(ProxyError::Protocol("MD5 auth message missing salt".into()));
            }
            let mut salt = [0u8; 4];
            salt.copy_from_slice(&payload[4..8]);
            Ok(AuthenticationMessage::Md5Password { salt })
        }

        AUTH_SCM_CREDENTIAL => Ok(AuthenticationMessage::ScmCredential),

        AUTH_GSS => Ok(AuthenticationMessage::Gss),

        AUTH_GSS_CONTINUE => {
            let data = payload[4..].to_vec();
            Ok(AuthenticationMessage::GssContinue { data })
        }

        AUTH_SSPI => Ok(AuthenticationMessage::Sspi),

        AUTH_SASL => {
            // Parse mechanism list (null-terminated strings, ending with empty string)
            let mut mechanisms = Vec::new();
            let mut offset = 4;

            while offset < payload.len() {
                let (mechanism, len) = read_cstring_from_buf(payload, offset)?;
                offset += len;

                if mechanism.is_empty() {
                    break;
                }
                mechanisms.push(mechanism);
            }

            Ok(AuthenticationMessage::Sasl { mechanisms })
        }

        AUTH_SASL_CONTINUE => {
            let data = payload[4..].to_vec();
            Ok(AuthenticationMessage::SaslContinue { data })
        }

        AUTH_SASL_FINAL => {
            let data = payload[4..].to_vec();
            Ok(AuthenticationMessage::SaslFinal { data })
        }

        _ => Err(ProxyError::Protocol(format!(
            "Unknown authentication type: {}",
            auth_type
        ))),
    }
}

/// Parse an error/notice response from payload.
pub fn parse_error_notice(payload: &[u8]) -> Result<ErrorNoticeResponse> {
    let mut response = ErrorNoticeResponse::new();
    let mut offset = 0;

    while offset < payload.len() {
        let field_type = payload[offset];
        offset += 1;

        if field_type == 0 {
            // End of fields
            break;
        }

        let (value, len) = read_cstring_from_buf(payload, offset)?;
        offset += len;

        response.set_field(field_type, &value);
    }

    Ok(response)
}

/// Parse a parameter status message from payload.
pub fn parse_parameter_status(payload: &[u8]) -> Result<ParameterStatus> {
    let (name, name_len) = read_cstring_from_buf(payload, 0)?;
    let (value, _) = read_cstring_from_buf(payload, name_len)?;

    Ok(ParameterStatus { name, value })
}

/// Parse a backend key data message from payload.
pub fn parse_backend_key_data(payload: &[u8]) -> Result<BackendKeyData> {
    if payload.len() < 8 {
        return Err(ProxyError::Protocol(
            "BackendKeyData message too short".into(),
        ));
    }

    let process_id = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let secret_key = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);

    Ok(BackendKeyData {
        process_id,
        secret_key,
    })
}

/// Parse a ready for query message from payload.
pub fn parse_ready_for_query(payload: &[u8]) -> Result<ReadyForQuery> {
    if payload.is_empty() {
        return Err(ProxyError::Protocol(
            "ReadyForQuery message too short".into(),
        ));
    }

    let status = TransactionStatus::from_byte(payload[0]).ok_or_else(|| {
        ProxyError::Protocol(format!("Invalid transaction status: {:02X}", payload[0]))
    })?;

    Ok(ReadyForQuery {
        transaction_status: status,
    })
}

/// Parse a row description message from payload.
pub fn parse_row_description(payload: &[u8]) -> Result<RowDescription> {
    if payload.len() < 2 {
        return Err(ProxyError::Protocol(
            "RowDescription message too short".into(),
        ));
    }

    let field_count = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let mut fields = Vec::with_capacity(field_count);
    let mut offset = 2;

    for _ in 0..field_count {
        let (name, name_len) = read_cstring_from_buf(payload, offset)?;
        offset += name_len;

        if offset + 18 > payload.len() {
            return Err(ProxyError::Protocol(
                "RowDescription field data truncated".into(),
            ));
        }

        let table_oid = u32::from_be_bytes([
            payload[offset],
            payload[offset + 1],
            payload[offset + 2],
            payload[offset + 3],
        ]);
        offset += 4;

        let column_id = u16::from_be_bytes([payload[offset], payload[offset + 1]]);
        offset += 2;

        let type_oid = u32::from_be_bytes([
            payload[offset],
            payload[offset + 1],
            payload[offset + 2],
            payload[offset + 3],
        ]);
        offset += 4;

        let type_size = i16::from_be_bytes([payload[offset], payload[offset + 1]]);
        offset += 2;

        let type_modifier = i32::from_be_bytes([
            payload[offset],
            payload[offset + 1],
            payload[offset + 2],
            payload[offset + 3],
        ]);
        offset += 4;

        let format = i16::from_be_bytes([payload[offset], payload[offset + 1]]);
        offset += 2;

        fields.push(FieldDescription {
            name,
            table_oid,
            column_id,
            type_oid,
            type_size,
            type_modifier,
            format,
        });
    }

    Ok(RowDescription { fields })
}

/// Parse a data row message from payload.
pub fn parse_data_row(payload: &[u8]) -> Result<DataRow> {
    if payload.len() < 2 {
        return Err(ProxyError::Protocol("DataRow message too short".into()));
    }

    let column_count = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let mut values = Vec::with_capacity(column_count);
    let mut offset = 2;

    for _ in 0..column_count {
        if offset + 4 > payload.len() {
            return Err(ProxyError::Protocol(
                "DataRow value length truncated".into(),
            ));
        }

        let value_len = i32::from_be_bytes([
            payload[offset],
            payload[offset + 1],
            payload[offset + 2],
            payload[offset + 3],
        ]);
        offset += 4;

        if value_len == -1 {
            // NULL value
            values.push(None);
        } else {
            let len = value_len as usize;
            if offset + len > payload.len() {
                return Err(ProxyError::Protocol("DataRow value data truncated".into()));
            }
            values.push(Some(payload[offset..offset + len].to_vec()));
            offset += len;
        }
    }

    Ok(DataRow { values })
}

/// Parse a command complete message from payload.
pub fn parse_command_complete(payload: &[u8]) -> Result<CommandComplete> {
    let (tag, _) = read_cstring_from_buf(payload, 0)?;
    Ok(CommandComplete { tag })
}

// ============================================================================
// Message Building (to payload)
// ============================================================================

/// Build a password message payload (for MD5 or cleartext password).
pub fn build_password_message(password: &str) -> Vec<u8> {
    let mut payload = password.as_bytes().to_vec();
    payload.push(0); // Null terminator
    payload
}

/// Build a SASL initial response payload.
pub fn build_sasl_initial_response(mechanism: &str, data: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();

    // Mechanism name (null-terminated)
    payload.extend_from_slice(mechanism.as_bytes());
    payload.push(0);

    // Data length (i32, -1 for no data)
    if data.is_empty() {
        payload.extend_from_slice(&(-1i32).to_be_bytes());
    } else {
        payload.extend_from_slice(&(data.len() as i32).to_be_bytes());
        payload.extend_from_slice(data);
    }

    payload
}

/// Build a SASL response payload (just the data).
pub fn build_sasl_response(data: &[u8]) -> Vec<u8> {
    data.to_vec()
}

/// Build an error response payload.
pub fn build_error_response(response: &ErrorNoticeResponse) -> Vec<u8> {
    let mut payload = Vec::new();

    for (&field_type, value) in &response.fields {
        payload.push(field_type);
        payload.extend_from_slice(value.as_bytes());
        payload.push(0); // Null terminator for value
    }

    payload.push(0); // Terminating null
    payload
}

/// Build a simple query payload.
pub fn build_query(query: &str) -> Vec<u8> {
    let mut payload = query.as_bytes().to_vec();
    payload.push(0); // Null terminator
    payload
}

/// Build a ready for query payload.
pub fn build_ready_for_query(status: TransactionStatus) -> Vec<u8> {
    vec![status.to_byte()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[tokio::test]
    async fn test_read_write_message() {
        let payload = b"test payload";
        let mut buf = Vec::new();

        // Write message
        write_message(&mut buf, MSG_QUERY, payload).await.unwrap();

        // Read it back
        let mut cursor = Cursor::new(&buf);
        let (msg_type, read_payload) = read_message(&mut cursor).await.unwrap();

        assert_eq!(msg_type, MSG_QUERY);
        assert_eq!(read_payload, payload);
    }

    #[tokio::test]
    async fn test_read_write_startup_message() {
        let msg = StartupMessage::with_database("testuser", "testdb");
        let mut buf = Vec::new();

        // Write startup message
        write_startup_message(&mut buf, &msg).await.unwrap();

        // Read it back
        let mut cursor = Cursor::new(&buf);
        let result = read_startup_message(&mut cursor).await.unwrap();

        match result {
            StartupMessageType::Startup(startup) => {
                assert_eq!(startup.protocol_version, PROTOCOL_VERSION_3_0);
                assert_eq!(startup.user(), Some("testuser"));
                assert_eq!(startup.database(), Some("testdb"));
            }
            _ => panic!("Expected Startup message"),
        }
    }

    #[tokio::test]
    async fn test_read_ssl_request() {
        let mut buf = Vec::new();
        write_ssl_request(&mut buf).await.unwrap();

        let mut cursor = Cursor::new(&buf);
        let result = read_startup_message(&mut cursor).await.unwrap();

        assert!(matches!(result, StartupMessageType::SSLRequest));
    }

    #[test]
    fn test_parse_authentication_ok() {
        let payload = [0, 0, 0, 0]; // AUTH_OK = 0
        let auth = parse_authentication(&payload).unwrap();
        assert!(matches!(auth, AuthenticationMessage::Ok));
    }

    #[test]
    fn test_parse_authentication_md5() {
        let mut payload = vec![0, 0, 0, 5]; // AUTH_MD5 = 5
        payload.extend_from_slice(&[0x12, 0x34, 0x56, 0x78]); // salt

        let auth = parse_authentication(&payload).unwrap();
        match auth {
            AuthenticationMessage::Md5Password { salt } => {
                assert_eq!(salt, [0x12, 0x34, 0x56, 0x78]);
            }
            _ => panic!("Expected Md5Password"),
        }
    }

    #[test]
    fn test_parse_authentication_sasl() {
        let mut payload = vec![0, 0, 0, 10]; // AUTH_SASL = 10
        payload.extend_from_slice(b"SCRAM-SHA-256\0\0");

        let auth = parse_authentication(&payload).unwrap();
        match auth {
            AuthenticationMessage::Sasl { mechanisms } => {
                assert_eq!(mechanisms, vec!["SCRAM-SHA-256"]);
            }
            _ => panic!("Expected Sasl"),
        }
    }

    #[test]
    fn test_parse_error_response() {
        let mut payload = Vec::new();
        payload.push(b'S'); // Severity
        payload.extend_from_slice(b"ERROR\0");
        payload.push(b'C'); // Code
        payload.extend_from_slice(b"42000\0");
        payload.push(b'M'); // Message
        payload.extend_from_slice(b"syntax error\0");
        payload.push(0); // Terminator

        let response = parse_error_notice(&payload).unwrap();
        assert_eq!(response.severity(), Some("ERROR"));
        assert_eq!(response.code(), Some("42000"));
        assert_eq!(response.message(), Some("syntax error"));
    }

    #[test]
    fn test_parse_parameter_status() {
        let mut payload = Vec::new();
        payload.extend_from_slice(b"server_version\0");
        payload.extend_from_slice(b"15.2\0");

        let status = parse_parameter_status(&payload).unwrap();
        assert_eq!(status.name, "server_version");
        assert_eq!(status.value, "15.2");
    }

    #[test]
    fn test_parse_backend_key_data() {
        let payload = [
            0x00, 0x01, 0x02, 0x03, // process_id
            0x04, 0x05, 0x06, 0x07, // secret_key
        ];

        let key_data = parse_backend_key_data(&payload).unwrap();
        assert_eq!(key_data.process_id, 0x00010203);
        assert_eq!(key_data.secret_key, 0x04050607);
    }

    #[test]
    fn test_parse_ready_for_query() {
        let payload = [b'I'];
        let ready = parse_ready_for_query(&payload).unwrap();
        assert_eq!(ready.transaction_status, TransactionStatus::Idle);

        let payload = [b'T'];
        let ready = parse_ready_for_query(&payload).unwrap();
        assert_eq!(ready.transaction_status, TransactionStatus::InTransaction);
    }

    #[test]
    fn test_parse_command_complete() {
        let payload = b"SELECT 5\0".to_vec();
        let complete = parse_command_complete(&payload).unwrap();
        assert_eq!(complete.tag, "SELECT 5");
    }

    #[test]
    fn test_build_password_message() {
        let payload = build_password_message("md5abc123");
        assert_eq!(payload, b"md5abc123\0");
    }

    #[test]
    fn test_build_sasl_initial_response() {
        let data = b"n,,n=user,r=nonce";
        let payload = build_sasl_initial_response("SCRAM-SHA-256", data);

        // Check mechanism
        assert!(payload.starts_with(b"SCRAM-SHA-256\0"));

        // Check length (after mechanism + null)
        let len_offset = "SCRAM-SHA-256".len() + 1;
        let len = i32::from_be_bytes([
            payload[len_offset],
            payload[len_offset + 1],
            payload[len_offset + 2],
            payload[len_offset + 3],
        ]);
        assert_eq!(len, data.len() as i32);
    }

    #[test]
    fn test_build_error_response() {
        let error = ErrorNoticeResponse::error("ERROR", "42000", "test error");
        let payload = build_error_response(&error);

        // Should contain all fields
        assert!(payload.contains(&b'S'));
        assert!(payload.contains(&b'C'));
        assert!(payload.contains(&b'M'));
        // Should end with null
        assert_eq!(payload.last(), Some(&0));
    }

    #[test]
    fn test_build_query() {
        let payload = build_query("SELECT 1");
        assert_eq!(payload, b"SELECT 1\0");
    }

    #[test]
    fn test_build_ready_for_query() {
        assert_eq!(build_ready_for_query(TransactionStatus::Idle), vec![b'I']);
        assert_eq!(
            build_ready_for_query(TransactionStatus::InTransaction),
            vec![b'T']
        );
        assert_eq!(build_ready_for_query(TransactionStatus::Failed), vec![b'E']);
    }
}
