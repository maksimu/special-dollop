// CSV Import for Database Handlers
//
// Implements file upload of CSV data to database tables via the Guacamole protocol.
// Supports cancellation via Ctrl+C and streaming for large files.
//
// Based on the KCM libguac-client-db import implementation.

use bytes::Bytes;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// State of the CSV import operation
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImportState {
    /// Waiting for file upload to begin
    WaitingForFile,
    /// Receiving file data
    Streaming,
    /// Writing data to database
    Writing,
    /// Import was cancelled
    Cancelled,
    /// Import completed
    Complete,
}

/// Parsed CSV data ready for database insertion
#[derive(Debug, Clone)]
pub struct CsvData {
    /// Column headers from first row
    pub headers: Vec<String>,
    /// Data rows
    pub rows: Vec<Vec<String>>,
}

impl CsvData {
    pub fn new() -> Self {
        Self {
            headers: Vec::new(),
            rows: Vec::new(),
        }
    }

    /// Number of rows (excluding header)
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

impl Default for CsvData {
    fn default() -> Self {
        Self::new()
    }
}

/// CSV importer for receiving file uploads from the client
pub struct CsvImporter {
    /// Unique stream index for this import
    stream_index: i32,

    /// Buffer for accumulating incoming data
    buffer: Vec<u8>,

    /// Flag to signal cancellation
    cancelled: Arc<AtomicBool>,

    /// Parsed CSV data
    parsed_data: Option<CsvData>,
}

impl CsvImporter {
    /// Create a new CSV importer
    pub fn new(stream_index: i32) -> Self {
        Self {
            stream_index,
            buffer: Vec::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
            parsed_data: None,
        }
    }

    /// Get a cancellation handle
    pub fn cancellation_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    /// Check if import has been cancelled
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Cancel the import
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Get the stream index
    pub fn stream_index(&self) -> i32 {
        self.stream_index
    }

    /// Create an ACK instruction to request file upload
    ///
    /// This tells the client we're ready to receive a file
    pub fn request_file_upload(&self) -> Bytes {
        // Send "ack" with status 0x0100 (GUAC_PROTOCOL_STATUS_SUCCESS) to request upload
        // The client should respond with a "file" instruction
        let instruction = format!(
            "3.ack,{}.{},0.,1.0;",
            self.stream_index.to_string().len(),
            self.stream_index,
        );
        Bytes::from(instruction)
    }

    /// Process incoming blob data
    ///
    /// Call this when receiving a "blob" instruction from the client
    pub fn receive_blob(&mut self, data: &[u8]) -> Result<(), String> {
        if self.is_cancelled() {
            return Err("Import cancelled".to_string());
        }

        // Decode base64 data
        let decoded = base64_decode(data)?;
        self.buffer.extend_from_slice(&decoded);

        Ok(())
    }

    /// Finish receiving data and parse the CSV
    ///
    /// Call this when receiving an "end" instruction from the client
    pub fn finish_receive(&mut self) -> Result<&CsvData, String> {
        if self.is_cancelled() {
            return Err("Import cancelled".to_string());
        }

        // Parse the CSV data
        let csv_string = String::from_utf8(std::mem::take(&mut self.buffer))
            .map_err(|e| format!("Invalid UTF-8 in CSV: {}", e))?;

        self.parsed_data = Some(parse_csv(&csv_string)?);

        self.parsed_data
            .as_ref()
            .ok_or_else(|| "No data parsed".to_string())
    }

    /// Get the parsed data (if available)
    pub fn get_data(&self) -> Option<&CsvData> {
        self.parsed_data.as_ref()
    }

    /// Generate INSERT statements for MySQL
    pub fn generate_mysql_inserts(&self, table_name: &str) -> Result<Vec<String>, String> {
        let data = self.parsed_data.as_ref().ok_or("No CSV data available")?;

        if data.headers.is_empty() {
            return Err("CSV has no headers".to_string());
        }

        let columns = data.headers.join("`, `");
        let mut statements = Vec::new();

        for row in &data.rows {
            if row.len() != data.headers.len() {
                continue; // Skip malformed rows
            }

            let values: Vec<String> = row.iter().map(|v| escape_sql_value(v)).collect();

            statements.push(format!(
                "INSERT INTO `{}` (`{}`) VALUES ({});",
                table_name,
                columns,
                values.join(", ")
            ));
        }

        Ok(statements)
    }

    /// Generate INSERT statements for PostgreSQL
    pub fn generate_postgres_inserts(&self, table_name: &str) -> Result<Vec<String>, String> {
        let data = self.parsed_data.as_ref().ok_or("No CSV data available")?;

        if data.headers.is_empty() {
            return Err("CSV has no headers".to_string());
        }

        let columns = data.headers.join("\", \"");
        let mut statements = Vec::new();

        for row in &data.rows {
            if row.len() != data.headers.len() {
                continue;
            }

            let values: Vec<String> = row.iter().map(|v| escape_sql_value(v)).collect();

            statements.push(format!(
                "INSERT INTO \"{}\" (\"{}\") VALUES ({});",
                table_name,
                columns,
                values.join(", ")
            ));
        }

        Ok(statements)
    }

    /// Generate INSERT statements for SQL Server
    pub fn generate_sqlserver_inserts(&self, table_name: &str) -> Result<Vec<String>, String> {
        let data = self.parsed_data.as_ref().ok_or("No CSV data available")?;

        if data.headers.is_empty() {
            return Err("CSV has no headers".to_string());
        }

        let columns = data.headers.join("], [");
        let mut statements = Vec::new();

        for row in &data.rows {
            if row.len() != data.headers.len() {
                continue;
            }

            let values: Vec<String> = row.iter().map(|v| escape_sql_value(v)).collect();

            statements.push(format!(
                "INSERT INTO [{}] ([{}]) VALUES ({});",
                table_name,
                columns,
                values.join(", ")
            ));
        }

        Ok(statements)
    }
}

/// Parse CSV content into headers and rows
fn parse_csv(content: &str) -> Result<CsvData, String> {
    let mut data = CsvData::new();
    let mut lines = content.lines().peekable();

    // Parse header row
    if let Some(header_line) = lines.next() {
        data.headers = parse_csv_line(header_line);
    } else {
        return Err("CSV is empty".to_string());
    }

    // Parse data rows
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        data.rows.push(parse_csv_line(line));
    }

    Ok(data)
}

/// Parse a single CSV line, handling quoted fields
fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current_field = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' if !in_quotes => {
                in_quotes = true;
            }
            '"' if in_quotes => {
                // Check for escaped quote ("")
                if chars.peek() == Some(&'"') {
                    current_field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            }
            ',' if !in_quotes => {
                fields.push(current_field.trim().to_string());
                current_field = String::new();
            }
            _ => {
                current_field.push(ch);
            }
        }
    }

    // Don't forget the last field
    fields.push(current_field.trim().to_string());

    fields
}

/// Escape a value for SQL insertion
fn escape_sql_value(value: &str) -> String {
    if value.eq_ignore_ascii_case("null") || value.is_empty() {
        return "NULL".to_string();
    }

    // Check if it's a number
    if value.parse::<f64>().is_ok() {
        return value.to_string();
    }

    // Escape single quotes and wrap in quotes
    let escaped = value.replace('\'', "''");
    format!("'{}'", escaped)
}

/// Base64 decode
fn base64_decode(data: &[u8]) -> Result<Vec<u8>, String> {
    const DECODE_TABLE: [i8; 128] = [
        -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
        -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, 62, -1, -1,
        -1, 63, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, -1, -1, -1, -1, -1, -1, -1, 0, 1, 2, 3, 4,
        5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, -1, -1, -1,
        -1, -1, -1, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45,
        46, 47, 48, 49, 50, 51, -1, -1, -1, -1, -1,
    ];

    let data_str = std::str::from_utf8(data).map_err(|e| format!("Invalid base64: {}", e))?;

    // Remove padding and whitespace
    let data_str = data_str.trim().trim_end_matches('=');

    let mut result = Vec::with_capacity(data_str.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits_collected = 0;

    for ch in data_str.bytes() {
        if ch >= 128 {
            return Err("Invalid base64 character".to_string());
        }
        let value = DECODE_TABLE[ch as usize];
        if value < 0 {
            continue; // Skip whitespace
        }

        buffer = (buffer << 6) | (value as u32);
        bits_collected += 6;

        if bits_collected >= 8 {
            bits_collected -= 8;
            result.push((buffer >> bits_collected) as u8);
            buffer &= (1 << bits_collected) - 1;
        }
    }

    Ok(result)
}

/// Parse a "file" instruction from the client
///
/// Returns (stream_index, mimetype, filename) if valid
#[allow(dead_code)]
pub fn parse_file_instruction(data: &[u8]) -> Option<(i32, String, String)> {
    let s = std::str::from_utf8(data).ok()?;

    // Format: 4.file,<stream-idx-len>.<stream-idx>,<mime-len>.<mime>,<name-len>.<name>;
    if !s.starts_with("4.file,") {
        return None;
    }

    let content = s.trim_start_matches("4.file,").trim_end_matches(';');
    let parts: Vec<&str> = content.split(',').collect();
    if parts.len() < 3 {
        return None;
    }

    // Parse stream index
    let stream_idx: i32 = parts[0].split('.').nth(1)?.parse().ok()?;

    // Parse mimetype
    let mimetype = parts[1].split('.').nth(1)?.to_string();

    // Parse filename
    let filename = parts[2].split('.').nth(1)?.to_string();

    Some((stream_idx, mimetype, filename))
}

/// Parse a "blob" instruction from the client
///
/// Returns (stream_index, data) if valid
#[allow(dead_code)]
pub fn parse_blob_instruction(data: &[u8]) -> Option<(i32, Vec<u8>)> {
    let s = std::str::from_utf8(data).ok()?;

    // Format: 4.blob,<stream-idx-len>.<stream-idx>,<data-len>.<base64-data>;
    if !s.starts_with("4.blob,") {
        return None;
    }

    let content = s.trim_start_matches("4.blob,").trim_end_matches(';');
    let parts: Vec<&str> = content.split(',').collect();
    if parts.len() < 2 {
        return None;
    }

    // Parse stream index
    let stream_idx: i32 = parts[0].split('.').nth(1)?.parse().ok()?;

    // Parse data (base64 encoded)
    let data_b64 = parts[1].split('.').nth(1)?;
    let decoded = base64_decode(data_b64.as_bytes()).ok()?;

    Some((stream_idx, decoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_csv_line_simple() {
        let result = parse_csv_line("a,b,c");
        assert_eq!(result, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_parse_csv_line_quoted() {
        let result = parse_csv_line("\"hello, world\",b,c");
        assert_eq!(result, vec!["hello, world", "b", "c"]);
    }

    #[test]
    fn test_parse_csv_line_escaped_quotes() {
        let result = parse_csv_line("\"say \"\"hello\"\"\",b");
        assert_eq!(result, vec!["say \"hello\"", "b"]);
    }

    #[test]
    fn test_parse_csv() {
        let csv = "name,age,city\nAlice,30,NYC\nBob,25,LA";
        let data = parse_csv(csv).unwrap();

        assert_eq!(data.headers, vec!["name", "age", "city"]);
        assert_eq!(data.rows.len(), 2);
        assert_eq!(data.rows[0], vec!["Alice", "30", "NYC"]);
        assert_eq!(data.rows[1], vec!["Bob", "25", "LA"]);
    }

    #[test]
    fn test_escape_sql_value() {
        assert_eq!(escape_sql_value("hello"), "'hello'");
        assert_eq!(escape_sql_value("it's"), "'it''s'");
        assert_eq!(escape_sql_value("NULL"), "NULL");
        assert_eq!(escape_sql_value(""), "NULL");
        assert_eq!(escape_sql_value("123"), "123");
        assert_eq!(escape_sql_value("45.67"), "45.67");
    }

    #[test]
    fn test_generate_mysql_inserts() {
        let mut importer = CsvImporter::new(1);
        importer.parsed_data = Some(CsvData {
            headers: vec!["name".to_string(), "age".to_string()],
            rows: vec![
                vec!["Alice".to_string(), "30".to_string()],
                vec!["Bob".to_string(), "25".to_string()],
            ],
        });

        let inserts = importer.generate_mysql_inserts("users").unwrap();
        assert_eq!(inserts.len(), 2);
        assert!(inserts[0].contains("INSERT INTO `users`"));
        assert!(inserts[0].contains("'Alice'"));
        assert!(inserts[0].contains("30"));
    }

    #[test]
    fn test_base64_decode() {
        let decoded = base64_decode(b"SGVsbG8=").unwrap();
        assert_eq!(decoded, b"Hello");

        let decoded = base64_decode(b"SGVsbG8sIFdvcmxkIQ==").unwrap();
        assert_eq!(decoded, b"Hello, World!");
    }

    #[test]
    fn test_cancellation() {
        let importer = CsvImporter::new(1);
        let handle = importer.cancellation_handle();

        assert!(!importer.is_cancelled());
        handle.store(true, Ordering::SeqCst);
        assert!(importer.is_cancelled());
    }
}
