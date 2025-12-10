// Database session recording support
//
// Provides recording infrastructure shared by all database handlers.

use guacr_handlers::{MultiFormatRecorder, RecordingConfig};
use guacr_terminal::QueryResult;
use log::{info, warn};
use std::collections::HashMap;

/// Initialize recording for a database handler
///
/// Returns an optional recorder based on configuration.
pub fn init_recording(
    recording_config: &RecordingConfig,
    params: &HashMap<String, String>,
    protocol: &str,
    cols: u16,
    rows: u16,
) -> Option<MultiFormatRecorder> {
    if !recording_config.is_enabled() {
        return None;
    }

    info!(
        "{}: Recording enabled - ses={}, asciicast={}, typescript={}",
        protocol,
        recording_config.is_ses_enabled(),
        recording_config.is_asciicast_enabled(),
        recording_config.is_typescript_enabled()
    );

    match MultiFormatRecorder::new(recording_config, params, protocol, cols, rows) {
        Ok(rec) => {
            info!("{}: Session recording initialized", protocol);
            Some(rec)
        }
        Err(e) => {
            warn!("{}: Failed to initialize recording: {}", protocol, e);
            None
        }
    }
}

/// Finalize recording session
pub fn finalize_recording(recorder: Option<MultiFormatRecorder>, protocol: &str) {
    if let Some(rec) = recorder {
        if let Err(e) = rec.finalize() {
            warn!("{}: Failed to finalize recording: {}", protocol, e);
        } else {
            info!("{}: Session recording finalized", protocol);
        }
    }
}

/// Record query input (if recording is enabled and includes keys)
pub fn record_query_input(
    recorder: &mut Option<MultiFormatRecorder>,
    recording_config: &RecordingConfig,
    query: &str,
) {
    if let Some(ref mut rec) = recorder {
        if recording_config.recording_include_keys {
            let input_line = format!("{}\r\n", query);
            let _ = rec.record_input(input_line.as_bytes());
        }
    }
}

/// Record query result output
pub fn record_query_output(recorder: &mut Option<MultiFormatRecorder>, result: &QueryResult) {
    if let Some(ref mut rec) = recorder {
        let output_text = format_query_result_for_recording(result);
        let _ = rec.record_output(output_text.as_bytes());
    }
}

/// Record error output
pub fn record_error_output(recorder: &mut Option<MultiFormatRecorder>, error: &str) {
    if let Some(ref mut rec) = recorder {
        let error_text = format!("ERROR: {}\r\n", error);
        let _ = rec.record_output(error_text.as_bytes());
    }
}

/// Format query result as text for recording
///
/// Produces a text representation of the query result suitable for
/// asciicast/typescript recording.
pub fn format_query_result_for_recording(result: &QueryResult) -> String {
    let mut output = String::new();

    if result.columns.is_empty() {
        if let Some(affected) = result.affected_rows {
            output.push_str(&format!("Query OK, {} row(s) affected\r\n", affected));
        } else {
            output.push_str("Query executed successfully\r\n");
        }
        return output;
    }

    // Calculate column widths
    let mut widths: Vec<usize> = result.columns.iter().map(|c| c.len()).collect();
    for row in &result.rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.len());
            }
        }
    }

    // Header
    let header: String = result
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{:width$}", c, width = widths.get(i).copied().unwrap_or(10)))
        .collect::<Vec<_>>()
        .join(" | ");
    output.push_str(&format!("{}\r\n", header));

    // Separator
    let sep: String = widths
        .iter()
        .map(|w| "-".repeat(*w))
        .collect::<Vec<_>>()
        .join("-+-");
    output.push_str(&format!("{}\r\n", sep));

    // Rows
    for row in &result.rows {
        let row_str: String = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                format!(
                    "{:width$}",
                    cell,
                    width = widths.get(i).copied().unwrap_or(10)
                )
            })
            .collect::<Vec<_>>()
            .join(" | ");
        output.push_str(&format!("{}\r\n", row_str));
    }

    // Row count
    output.push_str(&format!("({} row(s))\r\n", result.rows.len()));

    // Execution time if available
    if let Some(time_ms) = result.execution_time_ms {
        output.push_str(&format!("Time: {}ms\r\n", time_ms));
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_empty_result() {
        let result = QueryResult {
            columns: vec![],
            rows: vec![],
            affected_rows: Some(5),
            execution_time_ms: None,
        };
        let output = format_query_result_for_recording(&result);
        assert!(output.contains("5 row(s) affected"));
    }

    #[test]
    fn test_format_query_result() {
        let mut result = QueryResult::new(vec!["id".to_string(), "name".to_string()]);
        result.add_row(vec!["1".to_string(), "Alice".to_string()]);
        result.add_row(vec!["2".to_string(), "Bob".to_string()]);
        result.execution_time_ms = Some(42);

        let output = format_query_result_for_recording(&result);
        assert!(output.contains("id"));
        assert!(output.contains("name"));
        assert!(output.contains("Alice"));
        assert!(output.contains("Bob"));
        assert!(output.contains("(2 row(s))"));
        assert!(output.contains("Time: 42ms"));
    }
}
