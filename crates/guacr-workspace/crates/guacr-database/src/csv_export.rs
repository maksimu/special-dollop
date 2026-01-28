// CSV Export for Database Handlers
//
// Implements file download of query results as CSV via the Guacamole protocol.
// Supports cancellation via Ctrl+C and streaming for large result sets.
//
// Based on the KCM libguac-client-db export implementation.

use bytes::Bytes;
use guacr_terminal::QueryResult;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

/// State of the CSV export operation
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum ExportState {
    /// Waiting for client ACK to continue streaming
    AwaitingAck,
    /// Actively streaming data
    Streaming,
    /// Export was cancelled by user or error
    Cancelled,
    /// Export completed successfully
    Complete,
}

/// CSV exporter for streaming query results to the client as a file download
pub struct CsvExporter {
    /// Unique stream index for this export
    stream_index: i32,

    /// Buffer for accumulating CSV data before sending
    buffer: Vec<u8>,

    /// Maximum blob size for Guacamole protocol (6KB)
    max_blob_size: usize,

    /// Flag to signal cancellation
    cancelled: Arc<AtomicBool>,
}

impl CsvExporter {
    /// Create a new CSV exporter
    ///
    /// # Arguments
    /// * `stream_index` - Unique stream identifier (should be unique per connection)
    pub fn new(stream_index: i32) -> Self {
        Self {
            stream_index,
            buffer: Vec::with_capacity(6144),
            max_blob_size: 6144, // GUAC_PROTOCOL_BLOB_MAX_LENGTH
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Get a cancellation handle that can be used to cancel the export
    pub fn cancellation_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    /// Check if the export has been cancelled
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Cancel the export
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Start a file download by sending the Guacamole "file" instruction
    ///
    /// Returns the instruction to send to the client
    pub fn start_download(&self, filename: &str) -> Bytes {
        // 4.file,<stream-index>,<mimetype>,<filename>;
        let instruction = format!(
            "4.file,{}.{},8.text/csv,{}.{};",
            self.stream_index.to_string().len(),
            self.stream_index,
            filename.len(),
            filename
        );
        Bytes::from(instruction)
    }

    /// Export query results as CSV, yielding Guacamole blob instructions
    ///
    /// This streams the CSV data in chunks suitable for the Guacamole protocol.
    pub async fn export_query_result(
        &mut self,
        result: &QueryResult,
        to_client: &mpsc::Sender<Bytes>,
    ) -> Result<(), String> {
        if self.is_cancelled() {
            return Err("Export cancelled".to_string());
        }

        // Write header row
        self.write_csv_row(&result.columns)?;

        // Flush if buffer is getting full
        self.maybe_flush(to_client).await?;

        // Write data rows
        for row in &result.rows {
            if self.is_cancelled() {
                return Err("Export cancelled".to_string());
            }

            self.write_csv_row(row)?;
            self.maybe_flush(to_client).await?;
        }

        // Final flush
        self.flush(to_client).await?;

        // Send end instruction
        self.send_end(to_client).await?;

        Ok(())
    }

    /// Write a row of CSV data to the internal buffer
    fn write_csv_row(&mut self, fields: &[String]) -> Result<(), String> {
        for (i, field) in fields.iter().enumerate() {
            self.write_csv_field(field);
            if i < fields.len() - 1 {
                self.buffer.push(b',');
            }
        }
        self.buffer.extend_from_slice(b"\r\n");
        Ok(())
    }

    /// Write a single CSV field, escaping as needed
    fn write_csv_field(&mut self, field: &str) {
        // Check if field needs quoting
        let needs_quoting = field.contains(',')
            || field.contains('"')
            || field.contains('\n')
            || field.contains('\r');

        if needs_quoting {
            self.buffer.push(b'"');
            for ch in field.bytes() {
                if ch == b'"' {
                    self.buffer.push(b'"'); // Escape quotes by doubling
                }
                self.buffer.push(ch);
            }
            self.buffer.push(b'"');
        } else {
            self.buffer.extend_from_slice(field.as_bytes());
        }
    }

    /// Flush buffer if it exceeds the max blob size
    async fn maybe_flush(&mut self, to_client: &mpsc::Sender<Bytes>) -> Result<(), String> {
        if self.buffer.len() >= self.max_blob_size {
            self.flush(to_client).await?;
        }
        Ok(())
    }

    /// Flush the buffer, sending a blob instruction
    async fn flush(&mut self, to_client: &mpsc::Sender<Bytes>) -> Result<(), String> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        if self.is_cancelled() {
            return Err("Export cancelled".to_string());
        }

        // Send blob instruction
        // 4.blob,<stream-index>,<base64-data>;
        let data_base64 = base64_encode(&self.buffer);
        let instruction = format!(
            "4.blob,{}.{},{}.{};",
            self.stream_index.to_string().len(),
            self.stream_index,
            data_base64.len(),
            data_base64
        );

        to_client
            .send(Bytes::from(instruction))
            .await
            .map_err(|e| format!("Failed to send blob: {}", e))?;

        self.buffer.clear();
        Ok(())
    }

    /// Send the end instruction to complete the download
    async fn send_end(&self, to_client: &mpsc::Sender<Bytes>) -> Result<(), String> {
        // 3.end,<stream-index>;
        let instruction = format!(
            "3.end,{}.{};",
            self.stream_index.to_string().len(),
            self.stream_index
        );

        to_client
            .send(Bytes::from(instruction))
            .await
            .map_err(|e| format!("Failed to send end: {}", e))?;

        Ok(())
    }

    /// Send an ack instruction (used for responding to client acks)
    pub fn create_ack_instruction(&self, message: &str, status: u16) -> Bytes {
        // 3.ack,<stream-index>,<message>,<status>;
        let instruction = format!(
            "3.ack,{}.{},{}.{},{}.{};",
            self.stream_index.to_string().len(),
            self.stream_index,
            message.len(),
            message,
            status.to_string().len(),
            status
        );
        Bytes::from(instruction)
    }
}

/// Simple base64 encoding (without external dependency)
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut result = Vec::with_capacity(data.len().div_ceil(3) * 4);

    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;

        let n = (b0 << 16) | (b1 << 8) | b2;

        result.push(ALPHABET[((n >> 18) & 0x3F) as usize]);
        result.push(ALPHABET[((n >> 12) & 0x3F) as usize]);

        if chunk.len() > 1 {
            result.push(ALPHABET[((n >> 6) & 0x3F) as usize]);
        } else {
            result.push(b'=');
        }

        if chunk.len() > 2 {
            result.push(ALPHABET[(n & 0x3F) as usize]);
        } else {
            result.push(b'=');
        }
    }

    // Safe because we only used ASCII bytes
    String::from_utf8(result).unwrap()
}

/// Helper to generate a unique filename for CSV export
pub fn generate_csv_filename(query: &str, database_type: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    // Get timestamp
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Extract table name from query if possible
    let table_name = extract_table_name(query).unwrap_or("query");

    format!("{}_{}_export_{}.csv", database_type, table_name, timestamp)
}

/// Try to extract a table name from a SELECT query
fn extract_table_name(query: &str) -> Option<&str> {
    let query_upper = query.to_uppercase();

    // Look for FROM clause
    if let Some(from_pos) = query_upper.find(" FROM ") {
        let after_from = &query[from_pos + 6..];
        // Get the first word after FROM
        let table = after_from.split_whitespace().next()?;
        // Remove any trailing punctuation
        let table = table.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
        if !table.is_empty() {
            return Some(table);
        }
    }

    None
}

/// Parse an "ack" instruction from the client
///
/// Returns (stream_index, status) if valid
#[allow(dead_code)]
pub fn parse_ack_instruction(data: &[u8]) -> Option<(i32, u16)> {
    let s = std::str::from_utf8(data).ok()?;

    // Format: 3.ack,<stream-idx-len>.<stream-idx>,<msg-len>.<msg>,<status-len>.<status>;
    if !s.starts_with("3.ack,") {
        return None;
    }

    let parts: Vec<&str> = s.trim_end_matches(';').split(',').collect();
    if parts.len() < 3 {
        return None;
    }

    // Parse stream index (format: "len.value")
    let stream_part = parts[1];
    let stream_idx: i32 = stream_part.split('.').nth(1)?.parse().ok()?;

    // Parse status (format: "len.value")
    let status_part = parts.last()?;
    let status: u16 = status_part.split('.').nth(1)?.parse().ok()?;

    Some((stream_idx, status))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b"Hello"), "SGVsbG8=");
        assert_eq!(base64_encode(b"Hello, World!"), "SGVsbG8sIFdvcmxkIQ==");
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
    }

    #[test]
    fn test_csv_field_escaping() {
        let mut exporter = CsvExporter::new(1);

        // Simple field
        exporter.write_csv_field("hello");
        assert_eq!(String::from_utf8_lossy(&exporter.buffer), "hello");
        exporter.buffer.clear();

        // Field with comma
        exporter.write_csv_field("hello,world");
        assert_eq!(String::from_utf8_lossy(&exporter.buffer), "\"hello,world\"");
        exporter.buffer.clear();

        // Field with quote
        exporter.write_csv_field("say \"hello\"");
        assert_eq!(
            String::from_utf8_lossy(&exporter.buffer),
            "\"say \"\"hello\"\"\""
        );
        exporter.buffer.clear();

        // Field with newline
        exporter.write_csv_field("line1\nline2");
        assert_eq!(
            String::from_utf8_lossy(&exporter.buffer),
            "\"line1\nline2\""
        );
    }

    #[test]
    fn test_extract_table_name() {
        assert_eq!(extract_table_name("SELECT * FROM users"), Some("users"));
        assert_eq!(
            extract_table_name("SELECT id, name FROM products WHERE id > 5"),
            Some("products")
        );
        assert_eq!(extract_table_name("select * from ORDERS;"), Some("ORDERS"));
        assert_eq!(extract_table_name("INSERT INTO users"), None);
    }

    #[test]
    fn test_generate_filename() {
        let filename = generate_csv_filename("SELECT * FROM users", "mysql");
        assert!(filename.starts_with("mysql_users_export_"));
        assert!(filename.ends_with(".csv"));
    }

    #[test]
    fn test_start_download_instruction() {
        let exporter = CsvExporter::new(5);
        let instruction = exporter.start_download("test.csv");
        let s = String::from_utf8_lossy(&instruction);
        assert!(s.contains("file"));
        assert!(s.contains("text/csv"));
        assert!(s.contains("test.csv"));
    }

    #[test]
    fn test_cancellation() {
        let exporter = CsvExporter::new(1);
        let handle = exporter.cancellation_handle();

        assert!(!exporter.is_cancelled());

        handle.store(true, Ordering::SeqCst);
        assert!(exporter.is_cancelled());
    }
}
