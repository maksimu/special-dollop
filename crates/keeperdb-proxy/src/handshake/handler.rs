//! Handshake protocol handler.
//!
//! Implements the state machine for the Gateway handshake protocol.

use std::sync::Arc;
use std::time::Duration;

use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use uuid::Uuid;

use crate::auth::{DatabaseType, SessionConfig};
use crate::config::SessionSecurityConfig;
use crate::error::{ProxyError, Result};
use crate::tls::{TlsClientConfig, TlsVerifyMode};

use super::instruction::Instruction;
use super::store::{CredentialStore, HandshakeCredentials};

/// Default timeout for handshake operations
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Read buffer size
const READ_BUFFER_SIZE: usize = 4096;

/// Required parameters for the handshake
/// Note: "single_connection" (arg 12) is kept for backwards compatibility but ignored
const REQUIRED_ARGS: &[&str] = &[
    "target_host",       // 0
    "target_port",       // 1
    "username",          // 2
    "password",          // 3
    "database",          // 4
    "tls_enabled",       // 5
    "tls_ca_path",       // 6
    "tls_verify_mode",   // 7
    "session_uid",       // 8
    "max_duration_secs", // 9
    "idle_timeout_secs", // 10
    "max_queries",       // 11
    "single_connection", // 12 - LEGACY: kept for Gateway backwards compatibility, value ignored
    "require_token",     // 13
];

/// Result of a successful handshake.
#[derive(Debug)]
pub struct HandshakeResult {
    /// Session ID assigned to this connection
    pub session_id: Uuid,
    /// Database type from the select instruction
    pub database_type: DatabaseType,
    /// Target host from connect instruction
    pub target_host: String,
    /// Target port from connect instruction
    pub target_port: u16,
    /// Gateway-provided session UID for tunnel grouping (if provided)
    pub session_uid: Option<String>,
    /// TLS configuration from connect instruction
    pub tls_config: TlsClientConfig,
    /// Target database name (e.g. PDB service name for Oracle)
    pub database: Option<String>,
}

/// Handles the Gateway handshake protocol.
pub struct HandshakeHandler {
    /// TCP stream for the connection
    stream: TcpStream,
    /// Credential store for storing parsed credentials
    store: Arc<CredentialStore>,
    /// Session ID for this connection
    session_id: Uuid,
    /// Read buffer
    buffer: Vec<u8>,
    /// Current buffer position
    buffer_len: usize,
    /// Default session config from proxy config (used when handshake doesn't specify values)
    default_session_config: Option<SessionSecurityConfig>,
}

impl HandshakeHandler {
    /// Create a new handshake handler.
    pub fn new(stream: TcpStream, store: Arc<CredentialStore>) -> Self {
        Self {
            stream,
            store,
            session_id: Uuid::new_v4(),
            buffer: vec![0u8; READ_BUFFER_SIZE],
            buffer_len: 0,
            default_session_config: None,
        }
    }

    /// Create a new handshake handler with default session config from proxy config.
    ///
    /// The session config values will be used as defaults when the handshake
    /// doesn't provide explicit values for session parameters.
    pub fn with_config(
        stream: TcpStream,
        store: Arc<CredentialStore>,
        session_config: SessionSecurityConfig,
    ) -> Self {
        Self {
            stream,
            store,
            session_id: Uuid::new_v4(),
            buffer: vec![0u8; READ_BUFFER_SIZE],
            buffer_len: 0,
            default_session_config: Some(session_config),
        }
    }

    /// Create a handler with a specific session ID (for testing).
    pub fn with_session_id(
        stream: TcpStream,
        store: Arc<CredentialStore>,
        session_id: Uuid,
    ) -> Self {
        Self {
            stream,
            store,
            session_id,
            buffer: vec![0u8; READ_BUFFER_SIZE],
            buffer_len: 0,
            default_session_config: None,
        }
    }

    /// Perform the handshake and return the result with the stream.
    ///
    /// On success, returns the handshake result and the TCP stream
    /// (which can be used for subsequent database protocol traffic).
    pub async fn handle(mut self) -> Result<(HandshakeResult, TcpStream)> {
        debug!(session_id = %self.session_id, "Starting handshake");

        // 1. Read and validate "select" instruction
        let select_inst = self.read_instruction().await?;
        if !select_inst.is("select") {
            self.send_error(
                "PROTOCOL",
                &format!("Expected 'select', got '{}'", select_inst.opcode),
            )
            .await?;
            return Err(ProxyError::Protocol(format!(
                "Expected 'select' instruction, got '{}'",
                select_inst.opcode
            )));
        }

        let database_type = self.parse_database_type(&select_inst)?;
        debug!(session_id = %self.session_id, ?database_type, "Protocol selected");

        // 2. Send "args" instruction
        let args_inst = Instruction::args(REQUIRED_ARGS);
        self.write_instruction(&args_inst).await?;
        debug!(session_id = %self.session_id, "Sent args");

        // 3. Read "connect" instruction
        debug!(session_id = %self.session_id, "Waiting for connect instruction");
        let connect_inst = self.read_instruction().await?;
        debug!(session_id = %self.session_id, opcode = %connect_inst.opcode, num_args = connect_inst.args.len(), "Received instruction");
        if !connect_inst.is("connect") {
            self.send_error(
                "PROTOCOL",
                &format!("Expected 'connect', got '{}'", connect_inst.opcode),
            )
            .await?;
            return Err(ProxyError::Protocol(format!(
                "Expected 'connect' instruction, got '{}'",
                connect_inst.opcode
            )));
        }

        // 4. Parse credentials from connect args
        debug!(session_id = %self.session_id, "Parsing credentials from connect args");
        let credentials = match self.parse_credentials(database_type, &connect_inst) {
            Ok(creds) => {
                debug!(session_id = %self.session_id, target = %creds.target_host, port = creds.target_port, "Credentials parsed successfully");
                creds
            }
            Err(e) => {
                info!(session_id = %self.session_id, error = %e, "Failed to parse credentials");
                self.send_error("INVALID_CREDENTIALS", &e.to_string())
                    .await?;
                return Err(e);
            }
        };
        let target_host = credentials.target_host.clone();
        let target_port = credentials.target_port;
        let session_uid = credentials.session_uid.clone();
        let tls_config = credentials.tls_config.clone();
        let database = credentials.database.clone();

        // 5. Store credentials
        debug!(session_id = %self.session_id, "Storing credentials in credential store");
        self.store.store(self.session_id, credentials);
        debug!(session_id = %self.session_id, "Credentials stored successfully");

        // 6. Send "ready" instruction
        debug!(session_id = %self.session_id, "Sending ready instruction");
        let ready_inst = Instruction::ready(&self.session_id.to_string());
        match self.write_instruction(&ready_inst).await {
            Ok(()) => {
                debug!(session_id = %self.session_id, "Ready instruction sent successfully");
            }
            Err(e) => {
                info!(session_id = %self.session_id, error = %e, "Failed to send ready instruction");
                return Err(e);
            }
        }

        // Check for leftover data in buffer
        if self.buffer_len > 0 {
            warn!(
                session_id = %self.session_id,
                buffer_len = self.buffer_len,
                first_bytes = ?&self.buffer[..self.buffer_len.min(16)],
                "Handshake completed with leftover data in buffer - THIS DATA WILL BE LOST!"
            );
        }

        info!(
            session_id = %self.session_id,
            ?database_type,
            target = %target_host,
            port = target_port,
            session_uid = ?session_uid,
            buffer_remaining = self.buffer_len,
            "Handshake completed"
        );

        Ok((
            HandshakeResult {
                session_id: self.session_id,
                database_type,
                target_host,
                target_port,
                session_uid,
                tls_config,
                database,
            },
            self.stream,
        ))
    }

    /// Read an instruction from the stream.
    async fn read_instruction(&mut self) -> Result<Instruction> {
        loop {
            // Try to parse from existing buffer
            if self.buffer_len > 0 {
                match Instruction::parse(&self.buffer[..self.buffer_len]) {
                    Ok((inst, consumed)) => {
                        // Remove consumed bytes from buffer
                        self.buffer.copy_within(consumed..self.buffer_len, 0);
                        self.buffer_len -= consumed;
                        return Ok(inst);
                    }
                    Err(ProxyError::Protocol(msg)) if msg.contains("Incomplete") => {
                        // Need more data, fall through to read
                    }
                    Err(e) => return Err(e),
                }
            }

            // Read more data
            let n = timeout(
                HANDSHAKE_TIMEOUT,
                self.stream.read(&mut self.buffer[self.buffer_len..]),
            )
            .await
            .map_err(|_| ProxyError::Timeout("Handshake read timeout".into()))?
            .map_err(ProxyError::Io)?;

            if n == 0 {
                return Err(ProxyError::Protocol(
                    "Connection closed during handshake".into(),
                ));
            }

            self.buffer_len += n;
        }
    }

    /// Write an instruction to the stream.
    async fn write_instruction(&mut self, inst: &Instruction) -> Result<()> {
        let data = inst.encode();
        timeout(HANDSHAKE_TIMEOUT, self.stream.write_all(&data))
            .await
            .map_err(|_| ProxyError::Timeout("Handshake write timeout".into()))?
            .map_err(ProxyError::Io)?;

        timeout(HANDSHAKE_TIMEOUT, self.stream.flush())
            .await
            .map_err(|_| ProxyError::Timeout("Handshake flush timeout".into()))?
            .map_err(ProxyError::Io)?;

        Ok(())
    }

    /// Send an error instruction.
    async fn send_error(&mut self, code: &str, message: &str) -> Result<()> {
        let error_inst = Instruction::error(code, message);
        // Best effort - ignore errors when sending error
        let _ = self.write_instruction(&error_inst).await;
        Ok(())
    }

    /// Parse database type from select instruction.
    fn parse_database_type(&self, inst: &Instruction) -> Result<DatabaseType> {
        let protocol = inst
            .first_arg()
            .ok_or_else(|| ProxyError::Protocol("Missing database type in select".into()))?;

        match protocol.to_lowercase().as_str() {
            "mysql" => Ok(DatabaseType::MySQL),
            "postgresql" | "postgres" => Ok(DatabaseType::PostgreSQL),
            "sqlserver" | "mssql" => Ok(DatabaseType::SQLServer),
            "oracle" => Ok(DatabaseType::Oracle),
            other => Err(ProxyError::Protocol(format!(
                "Unsupported database type: {}. Supported: mysql, postgresql, sqlserver, oracle",
                other
            ))),
        }
    }

    /// Parse credentials from connect instruction.
    fn parse_credentials(
        &self,
        database_type: DatabaseType,
        inst: &Instruction,
    ) -> Result<HandshakeCredentials> {
        // Args are in the order specified by REQUIRED_ARGS
        let get_arg = |index: usize, name: &str| -> Result<&str> {
            inst.arg(index).ok_or_else(|| {
                ProxyError::Protocol(format!("Missing required parameter: {}", name))
            })
        };

        let target_host = get_arg(0, "target_host")?.to_string();
        if target_host.is_empty() {
            return Err(ProxyError::Protocol("target_host cannot be empty".into()));
        }

        let target_port: u16 = get_arg(1, "target_port")?
            .parse()
            .map_err(|_| ProxyError::Protocol("Invalid target_port".into()))?;

        let username = get_arg(2, "username")?.to_string();
        if username.is_empty() {
            return Err(ProxyError::Protocol("username cannot be empty".into()));
        }

        let password = get_arg(3, "password")?.to_string();

        // Optional parameters
        let database = inst.arg(4).filter(|s| !s.is_empty()).map(|s| s.to_string());

        let tls_enabled = inst
            .arg(5)
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let tls_ca_path = inst.arg(6).filter(|s| !s.is_empty()).map(|s| s.to_string());

        let tls_verify_mode = inst
            .arg(7)
            .map(parse_verify_mode)
            .unwrap_or(TlsVerifyMode::Verify);

        let session_uid = inst.arg(8).filter(|s| !s.is_empty()).map(|s| s.to_string());

        // Get defaults from config or use hardcoded fallbacks
        let defaults = self.default_session_config.as_ref();

        let max_duration_secs: u64 = inst
            .arg(9)
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| defaults.map(|c| c.max_duration_secs).unwrap_or(0));

        let idle_timeout_secs: u64 = inst
            .arg(10)
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| defaults.map(|c| c.idle_timeout_secs).unwrap_or(300));

        let max_queries: u64 = inst
            .arg(11)
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| defaults.map(|c| c.max_queries).unwrap_or(0));

        // arg 12 (single_connection) intentionally ignored - legacy field kept for backwards compatibility

        let require_token = inst
            .arg(13)
            .filter(|s| !s.is_empty())
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or_else(|| defaults.map(|c| c.require_token).unwrap_or(false));

        // Get max_connections_per_session and grace period from config defaults
        let max_connections_per_session: u32 =
            defaults.map(|c| c.max_connections_per_session).unwrap_or(0);

        let connection_grace_period_ms: u64 = defaults
            .map(|c| c.connection_grace_period_ms)
            .unwrap_or(1000);

        debug!(
            session_id = %self.session_id,
            max_connections_per_session,
            connection_grace_period_ms,
            max_queries,
            max_duration_secs,
            require_token,
            "Session config (from handshake or config defaults)"
        );

        // Build TLS config
        let tls_config = TlsClientConfig {
            enabled: tls_enabled,
            ca_path: tls_ca_path.map(PathBuf::from),
            verify_mode: tls_verify_mode,
            ..Default::default()
        };

        // Build session config
        let mut session_config = SessionConfig::default();
        if max_duration_secs > 0 {
            session_config =
                session_config.with_max_duration(Duration::from_secs(max_duration_secs));
        }
        if idle_timeout_secs > 0 {
            session_config =
                session_config.with_idle_timeout(Duration::from_secs(idle_timeout_secs));
        }
        if max_queries > 0 {
            session_config = session_config.with_max_queries(max_queries);
        }
        if max_connections_per_session > 0 {
            session_config =
                session_config.with_max_connections_per_session(max_connections_per_session);
        }
        if connection_grace_period_ms > 0 {
            session_config = session_config
                .with_connection_grace_period(Duration::from_millis(connection_grace_period_ms));
        }
        session_config.require_token = require_token;

        Ok(
            HandshakeCredentials::new(database_type, target_host, target_port, username, password)
                .with_database(database)
                .with_tls_config(tls_config)
                .with_session_config(session_config)
                .with_session_uid(session_uid),
        )
    }
}

/// Parse TLS verify mode from string.
fn parse_verify_mode(s: &str) -> TlsVerifyMode {
    match s.to_lowercase().as_str() {
        "none" | "disabled" | "false" => TlsVerifyMode::None,
        "ca" | "verify_ca" => TlsVerifyMode::VerifyCa,
        _ => TlsVerifyMode::Verify,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_verify_mode() {
        assert_eq!(parse_verify_mode("none"), TlsVerifyMode::None);
        assert_eq!(parse_verify_mode("disabled"), TlsVerifyMode::None);
        assert_eq!(parse_verify_mode("ca"), TlsVerifyMode::VerifyCa);
        assert_eq!(parse_verify_mode("verify_ca"), TlsVerifyMode::VerifyCa);
        assert_eq!(parse_verify_mode("verify"), TlsVerifyMode::Verify);
        assert_eq!(parse_verify_mode("full"), TlsVerifyMode::Verify);
    }
}
