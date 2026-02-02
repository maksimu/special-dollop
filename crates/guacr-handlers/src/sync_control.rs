// sync_control.rs - Guacamole sync flow control for all protocol handlers
//
// Implements bidirectional sync acknowledgment to prevent overwhelming slow clients.
// Server sends sync instructions with timestamps, then waits for client to echo back.
// Disconnects after multiple consecutive timeouts.
//
// This prevents frame buffering that causes lag and memory issues on slow connections.

use bytes::Bytes;
use std::time::Duration;
use tokio::sync::mpsc;

/// Manages sync flow control between server and client
///
/// Flow control prevents the server from overwhelming slow clients by:
/// 1. Sending a sync instruction with timestamp after each frame
/// 2. Waiting for client to acknowledge the sync (echo back the timestamp)
/// 3. Timing out after a configurable duration (default 15s)
/// 4. Disconnecting after multiple consecutive timeouts (default 3)
///
/// This matches Apache Guacamole's guacd behavior for compatibility.
pub struct SyncFlowControl {
    pending_sync_timestamp: Option<u64>,
    sync_timeout_count: u32,
    timeout_duration: Duration,
    max_consecutive_timeouts: u32,
}

impl SyncFlowControl {
    /// Create a new sync flow control manager with default settings
    ///
    /// Defaults match Apache Guacamole guacd:
    /// - 15 second timeout
    /// - 3 consecutive timeouts before disconnect
    pub fn new() -> Self {
        Self {
            pending_sync_timestamp: None,
            sync_timeout_count: 0,
            timeout_duration: Duration::from_secs(15),
            max_consecutive_timeouts: 3,
        }
    }

    /// Create with custom timeout settings
    ///
    /// # Arguments
    ///
    /// * `timeout_secs` - Seconds to wait for sync acknowledgment
    /// * `max_timeouts` - Number of consecutive timeouts before disconnect
    pub fn with_timeout(timeout_secs: u64, max_timeouts: u32) -> Self {
        Self {
            pending_sync_timestamp: None,
            sync_timeout_count: 0,
            timeout_duration: Duration::from_secs(timeout_secs),
            max_consecutive_timeouts: max_timeouts,
        }
    }

    /// Record that a sync was sent to the client
    ///
    /// Call this after sending a sync instruction. The timestamp should match
    /// what was sent in the sync instruction.
    ///
    /// # Arguments
    ///
    /// * `timestamp` - The timestamp sent in the sync instruction
    pub fn set_pending_sync(&mut self, timestamp: u64) {
        self.pending_sync_timestamp = Some(timestamp);
    }

    /// Wait for client to acknowledge the sync
    ///
    /// Blocks until:
    /// - Client sends back a sync with matching or newer timestamp (success)
    /// - Timeout expires (may allow retry if under max_consecutive_timeouts)
    /// - Client channel closes (error)
    ///
    /// # Arguments
    ///
    /// * `from_client` - Receiver for client messages
    /// * `sent_timestamp` - The timestamp that was sent (should match set_pending_sync)
    ///
    /// # Returns
    ///
    /// - `Ok(())` - Client acknowledged sync
    /// - `Err(msg)` - Client disconnected or exceeded timeout limit
    pub async fn wait_for_client_sync(
        &mut self,
        from_client: &mut mpsc::Receiver<Bytes>,
        sent_timestamp: u64,
    ) -> Result<(), String> {
        let timeout_result = tokio::time::timeout(self.timeout_duration, async {
            loop {
                match from_client.recv().await {
                    Some(msg) => {
                        // Try to parse as UTF-8 string for sync instruction check
                        if let Ok(msg_str) = std::str::from_utf8(&msg) {
                            if msg_str.starts_with("4.sync,") {
                                if let Some(ts) = self.parse_sync_timestamp(msg_str) {
                                    if ts >= sent_timestamp {
                                        // Client caught up with our sync
                                        self.sync_timeout_count = 0;
                                        return Ok(());
                                    }
                                }
                            }
                        }
                        // Not a matching sync, keep waiting
                    }
                    None => {
                        return Err("Client channel closed while waiting for sync".to_string());
                    }
                }
            }
        })
        .await;

        match timeout_result {
            Ok(Ok(())) => {
                // Client responded in time
                Ok(())
            }
            Ok(Err(e)) => {
                // Client channel closed
                Err(e)
            }
            Err(_) => {
                // Timeout
                self.sync_timeout_count += 1;

                if self.sync_timeout_count >= self.max_consecutive_timeouts {
                    Err(format!(
                        "Client not responding to sync ({} consecutive timeouts)",
                        self.sync_timeout_count
                    ))
                } else {
                    // Allow a few timeouts before giving up
                    log::warn!(
                        "Sync timeout {}/{} - continuing",
                        self.sync_timeout_count,
                        self.max_consecutive_timeouts
                    );
                    Ok(())
                }
            }
        }
    }

    /// Parse timestamp from sync instruction
    ///
    /// Sync instruction format: "4.sync,<len>.<timestamp>;"
    /// Example: "4.sync,13.1234567890123;"
    ///
    /// Uses zero-copy string slicing for efficiency.
    fn parse_sync_timestamp(&self, sync_instr: &str) -> Option<u64> {
        // Find the dot that separates length from timestamp
        let after_sync = sync_instr.strip_prefix("4.sync,")?;
        let dot_pos = after_sync.find('.')?;

        // Extract timestamp (between dot and semicolon)
        let timestamp_part = &after_sync[dot_pos + 1..];
        let semicolon_pos = timestamp_part.find(';')?;
        let timestamp_str = &timestamp_part[..semicolon_pos];

        timestamp_str.parse::<u64>().ok()
    }

    /// Check if currently waiting for a sync acknowledgment
    pub fn is_waiting_for_sync(&self) -> bool {
        self.pending_sync_timestamp.is_some()
    }

    /// Get the pending sync timestamp if any
    pub fn pending_timestamp(&self) -> Option<u64> {
        self.pending_sync_timestamp
    }

    /// Clear pending sync (e.g., after successful acknowledgment)
    pub fn clear_pending(&mut self) {
        self.pending_sync_timestamp = None;
    }

    /// Get the current timeout count
    pub fn timeout_count(&self) -> u32 {
        self.sync_timeout_count
    }

    /// Reset timeout count (e.g., after successful sync)
    pub fn reset_timeout_count(&mut self) {
        self.sync_timeout_count = 0;
    }

    /// Check if a sync timeout occurred and should disconnect
    ///
    /// Call this periodically (e.g., every second) to check if the pending sync has timed out.
    /// Returns true if the connection should be closed due to too many consecutive timeouts.
    pub fn check_timeout(&mut self, elapsed_since_sync: Duration) -> bool {
        if self.pending_sync_timestamp.is_none() {
            return false; // No pending sync
        }

        if elapsed_since_sync >= self.timeout_duration {
            // Timeout occurred
            self.sync_timeout_count += 1;
            self.pending_sync_timestamp = None; // Clear pending so we don't count it again

            if self.sync_timeout_count >= self.max_consecutive_timeouts {
                log::error!(
                    "Client not responding to sync ({} consecutive timeouts) - disconnecting",
                    self.sync_timeout_count
                );
                return true; // Should disconnect
            } else {
                log::warn!(
                    "Sync timeout {}/{} - continuing",
                    self.sync_timeout_count,
                    self.max_consecutive_timeouts
                );
            }
        }

        false // Continue
    }
}

impl Default for SyncFlowControl {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sync_timestamp() {
        let control = SyncFlowControl::new();

        let result = control.parse_sync_timestamp("4.sync,13.1234567890123;");
        assert_eq!(result, Some(1234567890123));

        let result = control.parse_sync_timestamp("4.sync,1.0;");
        assert_eq!(result, Some(0));

        let result = control.parse_sync_timestamp("invalid");
        assert_eq!(result, None);
    }

    #[test]
    fn test_pending_sync() {
        let mut control = SyncFlowControl::new();
        assert!(!control.is_waiting_for_sync());

        control.set_pending_sync(12345);
        assert!(control.is_waiting_for_sync());
        assert_eq!(control.pending_timestamp(), Some(12345));

        control.clear_pending();
        assert!(!control.is_waiting_for_sync());
    }

    #[test]
    fn test_timeout_count() {
        let mut control = SyncFlowControl::new();
        assert_eq!(control.timeout_count(), 0);

        control.sync_timeout_count = 2;
        assert_eq!(control.timeout_count(), 2);

        control.reset_timeout_count();
        assert_eq!(control.timeout_count(), 0);
    }
}
