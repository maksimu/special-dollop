use chrono::{DateTime, Utc};
use serde::Serialize;

/// SQL command type detected from query text.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SqlCommandType {
    Select,
    Insert,
    Update,
    Delete,
    Create,
    Alter,
    Drop,
    Truncate,
    Grant,
    Revoke,
    Begin,
    Commit,
    Rollback,
    Set,
    Show,
    Explain,
    Use,
    PreparedStatement,
    Other(String),
    /// Used when query text is unavailable (e.g., Oracle TNS binary protocol)
    Unknown,
}

impl SqlCommandType {
    /// Detect command type from the first keyword of a SQL query string.
    pub fn from_query_text(query: &str) -> Self {
        let trimmed = query.trim_start();
        // Find the first whitespace-delimited word
        let first_word = trimmed
            .split(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or("")
            .to_uppercase();

        match first_word.as_str() {
            "SELECT" => Self::Select,
            "INSERT" => Self::Insert,
            "UPDATE" => Self::Update,
            "DELETE" => Self::Delete,
            "CREATE" => Self::Create,
            "ALTER" => Self::Alter,
            "DROP" => Self::Drop,
            "TRUNCATE" => Self::Truncate,
            "GRANT" => Self::Grant,
            "REVOKE" => Self::Revoke,
            "BEGIN" | "START" => Self::Begin,
            "COMMIT" => Self::Commit,
            "ROLLBACK" | "ABORT" => Self::Rollback,
            "PREPARE" | "EXECUTE" | "DEALLOCATE" => Self::PreparedStatement,
            "SET" => Self::Set,
            "SHOW" => Self::Show,
            "EXPLAIN" | "DESCRIBE" | "DESC" => Self::Explain,
            "USE" => Self::Use,
            "" => Self::Unknown,
            other => Self::Other(other.to_string()),
        }
    }
}

/// A single query log entry with full audit context.
#[derive(Debug, Clone, Serialize)]
pub struct QueryLog {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub user_id: String,
    pub database: String,
    pub protocol: String,
    pub command_type: SqlCommandType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_text: Option<String>,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows_affected: Option<u64>,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub client_addr: String,
    pub server_addr: String,
}

/// Context carried through a session for populating query log entries.
#[derive(Debug, Clone)]
pub struct SessionContext {
    pub session_id: String,
    pub user_id: String,
    pub database: String,
    pub protocol: String,
    pub client_addr: String,
    pub server_addr: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_type_detection() {
        assert_eq!(
            SqlCommandType::from_query_text("SELECT * FROM users"),
            SqlCommandType::Select
        );
        assert_eq!(
            SqlCommandType::from_query_text("  select id from t"),
            SqlCommandType::Select
        );
        assert_eq!(
            SqlCommandType::from_query_text("INSERT INTO users VALUES (1)"),
            SqlCommandType::Insert
        );
        assert_eq!(
            SqlCommandType::from_query_text("UPDATE users SET name='a'"),
            SqlCommandType::Update
        );
        assert_eq!(
            SqlCommandType::from_query_text("DELETE FROM users WHERE id=1"),
            SqlCommandType::Delete
        );
        assert_eq!(
            SqlCommandType::from_query_text("CREATE TABLE t (id INT)"),
            SqlCommandType::Create
        );
        assert_eq!(
            SqlCommandType::from_query_text("DROP TABLE users"),
            SqlCommandType::Drop
        );
        assert_eq!(
            SqlCommandType::from_query_text("BEGIN"),
            SqlCommandType::Begin
        );
        assert_eq!(
            SqlCommandType::from_query_text("COMMIT"),
            SqlCommandType::Commit
        );
        assert_eq!(
            SqlCommandType::from_query_text("ROLLBACK"),
            SqlCommandType::Rollback
        );
    }

    #[test]
    fn test_command_type_case_insensitive() {
        assert_eq!(
            SqlCommandType::from_query_text("select * from t"),
            SqlCommandType::Select
        );
        assert_eq!(
            SqlCommandType::from_query_text("Select * From t"),
            SqlCommandType::Select
        );
    }

    #[test]
    fn test_command_type_with_leading_whitespace() {
        assert_eq!(
            SqlCommandType::from_query_text("   SELECT 1"),
            SqlCommandType::Select
        );
        assert_eq!(
            SqlCommandType::from_query_text("\n\tINSERT INTO t VALUES (1)"),
            SqlCommandType::Insert
        );
    }

    #[test]
    fn test_command_type_set_show_explain_use() {
        assert_eq!(
            SqlCommandType::from_query_text("SET autocommit = 1"),
            SqlCommandType::Set
        );
        assert_eq!(
            SqlCommandType::from_query_text("SHOW TABLES"),
            SqlCommandType::Show
        );
        assert_eq!(
            SqlCommandType::from_query_text("EXPLAIN SELECT 1"),
            SqlCommandType::Explain
        );
        assert_eq!(
            SqlCommandType::from_query_text("DESCRIBE users"),
            SqlCommandType::Explain
        );
        assert_eq!(
            SqlCommandType::from_query_text("DESC users"),
            SqlCommandType::Explain
        );
        assert_eq!(
            SqlCommandType::from_query_text("USE mydb"),
            SqlCommandType::Use
        );
    }

    #[test]
    fn test_command_type_unknown_and_other() {
        assert_eq!(SqlCommandType::from_query_text(""), SqlCommandType::Unknown);
        assert_eq!(
            SqlCommandType::from_query_text("   "),
            SqlCommandType::Unknown
        );
        assert_eq!(
            SqlCommandType::from_query_text("VACUUM"),
            SqlCommandType::Other("VACUUM".to_string())
        );
    }

    #[test]
    fn test_command_type_with_parens() {
        // e.g., "INSERT(col1, col2)" without space
        assert_eq!(
            SqlCommandType::from_query_text("INSERT(col1) VALUES(1)"),
            SqlCommandType::Insert
        );
    }

    #[test]
    fn test_query_log_serialization() {
        let log = QueryLog {
            timestamp: DateTime::parse_from_rfc3339("2026-02-24T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            session_id: "sess-001".to_string(),
            user_id: "admin".to_string(),
            database: "mydb".to_string(),
            protocol: "mysql".to_string(),
            command_type: SqlCommandType::Select,
            query_text: Some("SELECT * FROM users".to_string()),
            duration_ms: 42,
            rows_affected: Some(5),
            success: true,
            error_message: None,
            client_addr: "127.0.0.1:54321".to_string(),
            server_addr: "127.0.0.1:3306".to_string(),
        };

        let json = serde_json::to_string(&log).unwrap();
        assert!(json.contains("\"command_type\":\"SELECT\""));
        assert!(json.contains("\"duration_ms\":42"));
        assert!(json.contains("\"success\":true"));
        // error_message should be omitted (skip_serializing_if = None)
        assert!(!json.contains("error_message"));
    }

    #[test]
    fn test_query_log_with_error() {
        let log = QueryLog {
            timestamp: Utc::now(),
            session_id: "sess-002".to_string(),
            user_id: "admin".to_string(),
            database: "mydb".to_string(),
            protocol: "postgresql".to_string(),
            command_type: SqlCommandType::Delete,
            query_text: None,
            duration_ms: 3,
            rows_affected: None,
            success: false,
            error_message: Some("permission denied".to_string()),
            client_addr: "10.0.0.1:9999".to_string(),
            server_addr: "10.0.0.2:5432".to_string(),
        };

        let json = serde_json::to_string(&log).unwrap();
        assert!(json.contains("\"success\":false"));
        assert!(json.contains("\"error_message\":\"permission denied\""));
        // query_text should be omitted
        assert!(!json.contains("query_text"));
    }
}
