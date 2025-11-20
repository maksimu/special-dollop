use async_trait::async_trait;
use bytes::Bytes;
use guacr_handlers::{
    EventBasedHandler, EventCallback, HandlerError, HandlerStats, HealthStatus, InstructionSender,
    ProtocolHandler,
};
use log::{info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::mysql_executor::execute_mysql_query;
use crate::sql_terminal::SqlTerminal;

/// MySQL protocol handler
pub struct MySqlHandler {
    config: MySqlConfig,
}

#[derive(Debug, Clone)]
pub struct MySqlConfig {
    pub default_port: u16,
}

impl Default for MySqlConfig {
    fn default() -> Self {
        Self { default_port: 3306 }
    }
}

impl MySqlHandler {
    pub fn new(config: MySqlConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(MySqlConfig::default())
    }
}

#[async_trait]
impl ProtocolHandler for MySqlHandler {
    fn name(&self) -> &str {
        "mysql"
    }

    fn as_event_based(&self) -> Option<&dyn EventBasedHandler> {
        Some(self)
    }

    async fn connect(
        &self,
        params: HashMap<String, String>,
        to_client: mpsc::Sender<Bytes>,
        mut from_client: mpsc::Receiver<Bytes>,
    ) -> guacr_handlers::Result<()> {
        info!("MySQL handler starting");

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

        let _database = params.get("database");

        info!("MySQL connecting to {}@{}:{}", username, hostname, port);

        // Create SQL terminal
        let mut terminal = SqlTerminal::new(24, 80, "mysql> ")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        terminal
            .write_line("Connected to MySQL server.")
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

        // Connect to MySQL database
        let database = params.get("database").map(|s| s.as_str()).unwrap_or("");
        let password = params
            .get("password")
            .ok_or_else(|| HandlerError::MissingParameter("password".to_string()))?;

        let connection_url = if database.is_empty() {
            format!("mysql://{}:{}@{}:{}", username, password, hostname, port)
        } else {
            format!(
                "mysql://{}:{}@{}:{}/{}",
                username, password, hostname, port, database
            )
        };

        info!(
            "MySQL: Connecting to database: {}",
            connection_url.replace(password, "***")
        );

        // Create connection pool
        let pool = sqlx::mysql::MySqlPool::connect(&connection_url)
            .await
            .map_err(|e| {
                HandlerError::ConnectionFailed(format!("MySQL connection failed: {}", e))
            })?;

        info!("MySQL: Connected successfully");

        // Create query executor
        use crate::query_executor::QueryExecutor;
        let mut executor = QueryExecutor::new("mysql> ")
            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

        // Event loop - process queries
        while let Some(msg) = from_client.recv().await {
            // Process input
            match executor.process_input(&msg).await {
                Ok((needs_render, instructions, pending_query)) => {
                    // Execute query if Enter was pressed
                    if let Some(query) = pending_query {
                        info!("MySQL: Executing query: {}", query);

                        match execute_mysql_query(&pool, &query).await {
                            Ok(result) => {
                                // Render result
                                executor
                                    .terminal
                                    .write_table(&result.columns, &result.rows)
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                                executor
                                    .terminal
                                    .write_line(&format!("\n{} rows returned", result.rows.len()))
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            }
                            Err(e) => {
                                executor
                                    .terminal
                                    .write_error(&format!("MySQL error: {}", e))
                                    .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                            }
                        }
                        executor
                            .terminal
                            .write_prompt()
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;

                        // Re-render with results
                        let (_, result_instructions) = executor
                            .render_screen()
                            .await
                            .map_err(|e| HandlerError::ProtocolError(e.to_string()))?;
                        for instr in result_instructions {
                            to_client
                                .send(instr)
                                .await
                                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                        }
                        continue;
                    }

                    if needs_render {
                        // Send render instructions
                        for instr in instructions {
                            to_client
                                .send(instr)
                                .await
                                .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
                        }
                    }
                }
                Err(e) => {
                    warn!("MySQL: Query execution error: {}", e);
                    // Continue processing
                }
            }
        }

        info!("MySQL handler ended");
        Ok(())
    }

    async fn health_check(&self) -> guacr_handlers::Result<HealthStatus> {
        Ok(HealthStatus::Healthy)
    }

    async fn stats(&self) -> guacr_handlers::Result<HandlerStats> {
        Ok(HandlerStats::default())
    }
}

// Event-based handler implementation for zero-copy integration
#[async_trait]
impl EventBasedHandler for MySqlHandler {
    fn name(&self) -> &str {
        "mysql"
    }

    async fn connect_with_events(
        &self,
        params: HashMap<String, String>,
        callback: Arc<dyn EventCallback>,
        from_client: mpsc::Receiver<Bytes>,
    ) -> Result<(), HandlerError> {
        // Wrap the channel-based interface
        let (to_client, mut handler_rx) = mpsc::channel::<Bytes>(128);

        let sender = InstructionSender::new(callback);
        let sender_arc = Arc::new(sender);

        // Spawn task to forward channel messages to event callback (zero-copy)
        let sender_clone = Arc::clone(&sender_arc);
        tokio::spawn(async move {
            while let Some(msg) = handler_rx.recv().await {
                sender_clone.send(msg); // Zero-copy: Bytes is reference-counted
            }
        });

        // Call the existing channel-based connect method
        self.connect(params, to_client, from_client).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mysql_handler_new() {
        let handler = MySqlHandler::with_defaults();
        assert_eq!(
            <_ as guacr_handlers::ProtocolHandler>::name(&handler),
            "mysql"
        );
    }

    #[test]
    fn test_mysql_config() {
        let config = MySqlConfig::default();
        assert_eq!(config.default_port, 3306);
    }
}
