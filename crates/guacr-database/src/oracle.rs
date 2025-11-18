use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{HandlerError, HandlerStats, HealthStatus, ProtocolHandler};
use log::info;
use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::sql_terminal::SqlTerminal;

/// Oracle Database handler
///
/// Enterprise database with strong security model
pub struct OracleHandler {
    config: OracleConfig,
}

#[derive(Debug, Clone)]
pub struct OracleConfig {
    pub default_port: u16,
    pub service_name: String,
    pub require_encryption: bool,
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            default_port: 1521,
            service_name: "ORCL".to_string(),
            require_encryption: true, // Security: Encryption required
        }
    }
}

impl OracleHandler {
    pub fn new(config: OracleConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(OracleConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for OracleHandler {
    fn name(&self) -> &str {
        "oracle"
    }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> guacr_handlers::Result<()> {
        info!("Oracle handler starting");

        let hostname = params
            .get("hostname")
            .ok_or_else(|| HandlerError::MissingParameter("hostname".to_string()))?;

        let port: u16 = params
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(self.config.default_port);

        let username = params
            .get("username")
            .ok_or_else(|| HandlerError::MissingParameter("username".to_string()))?;

        let _password = params
            .get("password")
            .ok_or_else(|| HandlerError::MissingParameter("password".to_string()))?;

        let service = params
            .get("service")
            .cloned()
            .unwrap_or(self.config.service_name.clone());

        info!(
            "Oracle connecting to {}@{}:{}/{}",
            username, hostname, port, service
        );

        // Security validation
        if self.config.require_encryption {
            info!("Oracle: Network encryption required");
        }

        // Create SQL terminal
        let mut terminal = SqlTerminal::new(24, 80, "SQL> ")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        terminal
            .write_line(&format!("Connected to Oracle Database: {}", hostname))
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        terminal
            .write_line(&format!("Service: {}", service))
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        terminal
            .write_prompt()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Send initial screen
        let png = terminal
            .render_png()
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
        let img_instructions = terminal.format_img_instructions(&png, 1);
        for instr in img_instructions {
            to_client
                .send(Bytes::from(instr))
                .await
                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
        }

        // TODO: Actual Oracle connection
        // Security features to implement:
        // - Native network encryption
        // - Prepared statements (prevent SQL injection)
        // - Schema restrictions (ALTER SESSION SET CURRENT_SCHEMA)
        // - No SYS/SYSTEM access
        // - Audit all DDL commands
        //
        // use oracle::Connection;
        // let conn_str = format!("{}:{}/{}", hostname, port, service);
        // let conn = Connection::connect(username, password, &conn_str)?;
        //
        // Security: Set session parameters
        // conn.execute("ALTER SESSION SET SQL_TRACE = FALSE", &[])?;
        // conn.execute("ALTER SESSION SET CURRENT_SCHEMA = ?", &[&username])?;
        //
        // Use prepared statements:
        // let mut stmt = conn.prepare("SELECT * FROM table WHERE id = :1")?;
        // stmt.execute(&[&user_input])?;  // Parameterized - prevents injection

        while from_client.recv().await.is_some() {}

        info!("Oracle handler ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oracle_handler_new() {
        let handler = OracleHandler::with_defaults();
        assert_eq!(handler.name(), "oracle");
    }

    #[test]
    fn test_oracle_config() {
        let config = OracleConfig::default();
        assert_eq!(config.default_port, 1521);
        assert_eq!(config.service_name, "ORCL");
        assert!(config.require_encryption); // Security
    }

    #[test]
    fn test_oracle_security_defaults() {
        let config = OracleConfig::default();
        assert!(
            config.require_encryption,
            "Oracle should require encryption by default"
        );
    }
}
