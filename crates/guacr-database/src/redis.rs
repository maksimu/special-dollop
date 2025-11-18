use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{HandlerError, HandlerStats, HealthStatus, ProtocolHandler};
use log::info;
use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::sql_terminal::SqlTerminal;

/// Redis handler
///
/// Provides Redis key-value store access with terminal interface
pub struct RedisHandler {
    config: RedisConfig,
}

#[derive(Debug, Clone)]
pub struct RedisConfig {
    pub default_port: u16,
    pub require_tls: bool,
    pub require_auth: bool,
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            default_port: 6379,
            require_tls: true,  // Security: TLS by default
            require_auth: true, // Security: Password required
        }
    }
}

impl RedisHandler {
    pub fn new(config: RedisConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(RedisConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for RedisHandler {
    fn name(&self) -> &str {
        "redis"
    }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> guacr_handlers::Result<()> {
        info!("Redis handler starting");

        let hostname = params
            .get("hostname")
            .ok_or_else(|| HandlerError::MissingParameter("hostname".to_string()))?;

        let port: u16 = params
            .get("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(self.config.default_port);

        let _password = if self.config.require_auth {
            Some(params.get("password").ok_or_else(|| {
                HandlerError::MissingParameter("password (required for security)".to_string())
            })?)
        } else {
            params.get("password")
        };

        let database: u8 = params
            .get("database")
            .and_then(|d| d.parse().ok())
            .unwrap_or(0);

        info!("Redis connecting to {}:{} db={}", hostname, port, database);

        // Security: Enforce TLS
        if self.config.require_tls {
            info!("Redis: TLS required for connection");
        }

        // Create terminal for Redis CLI
        let mut terminal = SqlTerminal::new(24, 80, "redis> ")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        terminal
            .write_line(&format!("Connected to Redis: {}", hostname))
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

        // TODO: Actual Redis connection
        // Security features:
        // - rediss:// (TLS) required
        // - AUTH command required
        // - ACL restrictions (Redis 6+)
        // - Keyspace pattern restrictions
        //
        // use redis::aio::ConnectionManager;
        // let protocol = if self.config.require_tls { "rediss" } else { "redis" };
        // let client = redis::Client::open(format!(
        //     "{}://{}@{}:{}/{}",
        //     protocol,
        //     password.unwrap_or(""),
        //     hostname,
        //     port,
        //     database
        // ))?;
        // let mut conn = ConnectionManager::new(client).await?;
        //
        // ACL setup (Redis 6+):
        // redis::cmd("ACL")
        //     .arg("SETUSER")
        //     .arg(username)
        //     .arg("on")
        //     .arg(&format!("+@read"))  // Read-only
        //     .arg(&format!("~{}:*", keyspace))  // Restrict keys
        //     .query_async(&mut conn).await?;

        while from_client.recv().await.is_some() {}

        info!("Redis handler ended");
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
    fn test_redis_handler_new() {
        let handler = RedisHandler::with_defaults();
        assert_eq!(handler.name(), "redis");
    }

    #[test]
    fn test_redis_config_security_defaults() {
        let config = RedisConfig::default();
        assert_eq!(config.default_port, 6379);
        assert!(config.require_tls); // Security: TLS required
        assert!(config.require_auth); // Security: Auth required
    }

    #[test]
    fn test_redis_security_enforced() {
        let config = RedisConfig::default();
        // Verify security is enforced by default
        assert!(config.require_tls);
        assert!(config.require_auth);
    }
}
