//! Integration tests for database handlers
//!
//! These tests require running databases. Start them with:
//!   docker-compose -f docker-compose.test.yml up -d
//!
//! Run tests with:
//!   cargo test --package guacr-database --test integration_test
//!
//! Connection details:
//!   MySQL:      localhost:13306, testuser/testpassword, testdb
//!   MariaDB:    localhost:13307, testuser/testpassword, testdb
//!   PostgreSQL: localhost:15432, testuser/testpassword, testdb
//!   SQL Server: localhost:11433, sa/TestPassword123!
//!   MongoDB:    localhost:17017, testuser/testpassword, testdb
//!   Redis:      localhost:16379, password: testpassword

use std::time::Duration;
use tokio::time::timeout;

/// Test connection timeout
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Check if a port is open (database is running)
async fn port_is_open(host: &str, port: u16) -> bool {
    timeout(
        Duration::from_secs(1),
        tokio::net::TcpStream::connect(format!("{}:{}", host, port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

mod mysql_tests {
    use super::*;

    const HOST: &str = "127.0.0.1";
    const PORT: u16 = 13306;

    async fn skip_if_not_available() -> bool {
        if !port_is_open(HOST, PORT).await {
            eprintln!(
                "Skipping MySQL tests - database not available on {}:{}",
                HOST, PORT
            );
            return true;
        }
        false
    }

    #[tokio::test]
    async fn test_mysql_connection() {
        if skip_if_not_available().await {
            return;
        }

        // Initialize rustls
        let _ = rustls::crypto::ring::default_provider().install_default();

        let url = format!("mysql://testuser:testpassword@{}:{}/testdb", HOST, PORT);
        let opts = mysql_async::Opts::from_url(&url).expect("Invalid URL");
        let pool = mysql_async::Pool::new(opts);

        let result = timeout(CONNECT_TIMEOUT, pool.get_conn()).await;
        assert!(result.is_ok(), "Connection timed out");
        assert!(result.unwrap().is_ok(), "Failed to connect to MySQL");
    }

    #[tokio::test]
    async fn test_mysql_query() {
        if skip_if_not_available().await {
            return;
        }

        let _ = rustls::crypto::ring::default_provider().install_default();

        let url = format!("mysql://testuser:testpassword@{}:{}/testdb", HOST, PORT);
        let opts = mysql_async::Opts::from_url(&url).expect("Invalid URL");
        let pool = mysql_async::Pool::new(opts);

        use mysql_async::prelude::*;
        let mut conn = pool.get_conn().await.expect("Failed to connect");

        let result: Vec<mysql_async::Row> = conn
            .query("SELECT * FROM users")
            .await
            .expect("Query failed");

        assert!(!result.is_empty(), "Expected users in table");
        println!("MySQL: Found {} users", result.len());
    }

    #[tokio::test]
    async fn test_mysql_read_only_blocks_insert() {
        use guacr_database::{check_query_allowed, DatabaseSecuritySettings};
        use guacr_handlers::HandlerSecuritySettings;

        let settings = DatabaseSecuritySettings {
            base: HandlerSecuritySettings {
                read_only: true,
                ..Default::default()
            },
            ..Default::default()
        };

        // SELECT should be allowed
        assert!(check_query_allowed("SELECT * FROM users", &settings).is_ok());

        // INSERT should be blocked
        let result = check_query_allowed("INSERT INTO users (username) VALUES ('test')", &settings);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("read-only"));
    }
}

mod postgres_tests {
    use super::*;

    const HOST: &str = "127.0.0.1";
    const PORT: u16 = 15432;

    async fn skip_if_not_available() -> bool {
        if !port_is_open(HOST, PORT).await {
            eprintln!(
                "Skipping PostgreSQL tests - database not available on {}:{}",
                HOST, PORT
            );
            return true;
        }
        false
    }

    #[tokio::test]
    async fn test_postgres_connection() {
        if skip_if_not_available().await {
            return;
        }

        use sqlx::postgres::PgPoolOptions;

        let url = format!("postgres://testuser:testpassword@{}:{}/testdb", HOST, PORT);
        let result = timeout(
            CONNECT_TIMEOUT,
            PgPoolOptions::new().max_connections(1).connect(&url),
        )
        .await;

        assert!(result.is_ok(), "Connection timed out");
        assert!(result.unwrap().is_ok(), "Failed to connect to PostgreSQL");
    }

    #[tokio::test]
    async fn test_postgres_query() {
        if skip_if_not_available().await {
            return;
        }

        use sqlx::postgres::PgPoolOptions;

        let url = format!("postgres://testuser:testpassword@{}:{}/testdb", HOST, PORT);
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("Failed to connect");

        let rows: Vec<sqlx::postgres::PgRow> = sqlx::query("SELECT * FROM users")
            .fetch_all(&pool)
            .await
            .expect("Query failed");

        assert!(!rows.is_empty(), "Expected users in table");
        println!("PostgreSQL: Found {} users", rows.len());
    }

    #[tokio::test]
    async fn test_postgres_jsonb() {
        if skip_if_not_available().await {
            return;
        }

        use sqlx::postgres::PgPoolOptions;
        use sqlx::Row;

        let url = format!("postgres://testuser:testpassword@{}:{}/testdb", HOST, PORT);
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("Failed to connect");

        let row: sqlx::postgres::PgRow =
            sqlx::query("SELECT metadata FROM products WHERE name = 'Widget A'")
                .fetch_one(&pool)
                .await
                .expect("Query failed");

        let metadata: serde_json::Value = row.get("metadata");
        assert_eq!(metadata["color"], "red");
    }
}

mod redis_tests {
    use super::*;

    const HOST: &str = "127.0.0.1";
    const PORT: u16 = 16379;

    async fn skip_if_not_available() -> bool {
        if !port_is_open(HOST, PORT).await {
            eprintln!(
                "Skipping Redis tests - database not available on {}:{}",
                HOST, PORT
            );
            return true;
        }
        false
    }

    #[tokio::test]
    async fn test_redis_connection() {
        if skip_if_not_available().await {
            return;
        }

        let url = format!("redis://:testpassword@{}:{}", HOST, PORT);
        let client = redis::Client::open(url).expect("Invalid URL");

        let result = timeout(CONNECT_TIMEOUT, client.get_multiplexed_async_connection()).await;
        assert!(result.is_ok(), "Connection timed out");
        assert!(result.unwrap().is_ok(), "Failed to connect to Redis");
    }

    #[tokio::test]
    async fn test_redis_set_get() {
        if skip_if_not_available().await {
            return;
        }

        use redis::AsyncCommands;

        let url = format!("redis://:testpassword@{}:{}", HOST, PORT);
        let client = redis::Client::open(url).expect("Invalid URL");
        let mut con = client
            .get_multiplexed_async_connection()
            .await
            .expect("Failed to connect");

        // Set a value
        let _: () = con.set("test_key", "test_value").await.expect("SET failed");

        // Get it back
        let value: String = con.get("test_key").await.expect("GET failed");
        assert_eq!(value, "test_value");

        // Clean up
        let _: () = con.del("test_key").await.expect("DEL failed");
    }
}

mod mongodb_tests {
    use super::*;

    const HOST: &str = "127.0.0.1";
    const PORT: u16 = 17017;

    async fn skip_if_not_available() -> bool {
        if !port_is_open(HOST, PORT).await {
            eprintln!(
                "Skipping MongoDB tests - database not available on {}:{}",
                HOST, PORT
            );
            return true;
        }
        false
    }

    #[tokio::test]
    async fn test_mongodb_connection() {
        if skip_if_not_available().await {
            return;
        }

        let url = format!("mongodb://testuser:testpassword@{}:{}", HOST, PORT);
        let result = timeout(CONNECT_TIMEOUT, mongodb::Client::with_uri_str(&url)).await;

        assert!(result.is_ok(), "Connection timed out");
        assert!(result.unwrap().is_ok(), "Failed to connect to MongoDB");
    }

    #[tokio::test]
    async fn test_mongodb_query() {
        if skip_if_not_available().await {
            return;
        }

        use futures_util::StreamExt;
        use mongodb::bson::doc;

        let url = format!("mongodb://testuser:testpassword@{}:{}", HOST, PORT);
        let client = mongodb::Client::with_uri_str(&url)
            .await
            .expect("Failed to connect");

        let db = client.database("testdb");
        let collection = db.collection::<mongodb::bson::Document>("users");

        let mut cursor = collection.find(doc! {}).await.expect("Find failed");

        let mut count = 0;
        while let Some(result) = cursor.next().await {
            assert!(result.is_ok());
            count += 1;
        }

        println!("MongoDB: Found {} users", count);
    }
}

mod sqlserver_tests {
    use super::*;

    const HOST: &str = "127.0.0.1";
    const PORT: u16 = 11433;

    async fn skip_if_not_available() -> bool {
        if !port_is_open(HOST, PORT).await {
            eprintln!(
                "Skipping SQL Server tests - database not available on {}:{}",
                HOST, PORT
            );
            return true;
        }
        false
    }

    #[tokio::test]
    async fn test_sqlserver_connection() {
        if skip_if_not_available().await {
            return;
        }

        use tiberius::{AuthMethod, Client, Config};
        use tokio::net::TcpStream;
        use tokio_util::compat::TokioAsyncWriteCompatExt;

        let mut config = Config::new();
        config.host(HOST);
        config.port(PORT);
        config.authentication(AuthMethod::sql_server("sa", "TestPassword123!"));
        config.trust_cert();

        let tcp = TcpStream::connect(config.get_addr())
            .await
            .expect("TCP connect failed");
        tcp.set_nodelay(true).unwrap();

        let result = timeout(CONNECT_TIMEOUT, Client::connect(config, tcp.compat_write())).await;
        assert!(result.is_ok(), "Connection timed out");
        assert!(result.unwrap().is_ok(), "Failed to connect to SQL Server");
    }
}

mod csv_export_tests {
    use guacr_database::CsvExporter;
    use guacr_terminal::QueryResult;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_csv_export_basic() {
        let (tx, mut rx) = mpsc::channel(100);

        let result = QueryResult {
            columns: vec!["id".to_string(), "name".to_string(), "value".to_string()],
            rows: vec![
                vec!["1".to_string(), "Alice".to_string(), "100".to_string()],
                vec!["2".to_string(), "Bob".to_string(), "200".to_string()],
            ],
            affected_rows: None,
            execution_time_ms: None,
        };

        let mut exporter = CsvExporter::new(1);

        // Start download
        let file_instr = exporter.start_download("test.csv");
        tx.send(file_instr).await.unwrap();

        // Export data
        exporter.export_query_result(&result, &tx).await.unwrap();

        drop(tx);

        // Collect all messages
        let mut messages = Vec::new();
        while let Some(msg) = rx.recv().await {
            messages.push(String::from_utf8_lossy(&msg).to_string());
        }

        // Verify we got file, blob, and end instructions
        assert!(messages.iter().any(|m| m.contains("file")));
        assert!(messages.iter().any(|m| m.contains("blob")));
        assert!(messages.iter().any(|m| m.contains("end")));
    }

    #[tokio::test]
    async fn test_csv_export_special_characters() {
        let (tx, mut rx) = mpsc::channel(100);

        let result = QueryResult {
            columns: vec!["text".to_string()],
            rows: vec![
                vec!["Hello, World".to_string()],  // Contains comma
                vec!["Say \"Hello\"".to_string()], // Contains quotes
                vec!["Line1\nLine2".to_string()],  // Contains newline
            ],
            affected_rows: None,
            execution_time_ms: None,
        };

        let mut exporter = CsvExporter::new(2);
        tx.send(exporter.start_download("special.csv"))
            .await
            .unwrap();
        exporter.export_query_result(&result, &tx).await.unwrap();
        drop(tx);

        // Collect messages - the blob should contain properly escaped CSV
        let mut has_blob = false;
        while let Some(msg) = rx.recv().await {
            let s = String::from_utf8_lossy(&msg);
            if s.contains("blob") {
                has_blob = true;
            }
        }
        assert!(has_blob);
    }
}

mod csv_import_tests {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use guacr_database::CsvImporter;

    /// Helper to base64 encode data for testing
    fn encode_csv(csv: &str) -> Vec<u8> {
        BASE64.encode(csv.as_bytes()).into_bytes()
    }

    #[test]
    fn test_csv_import_parse_simple() {
        let mut importer = CsvImporter::new(1);

        // Simulate receiving base64-encoded CSV data (as Guacamole sends it)
        let csv = "name,age,city\nAlice,30,NYC\nBob,25,LA";
        importer.receive_blob(&encode_csv(csv)).unwrap();

        let data = importer.finish_receive().unwrap();

        assert_eq!(data.headers, vec!["name", "age", "city"]);
        assert_eq!(data.row_count(), 2);
        assert_eq!(data.rows[0], vec!["Alice", "30", "NYC"]);
        assert_eq!(data.rows[1], vec!["Bob", "25", "LA"]);
    }

    #[test]
    fn test_csv_import_quoted_fields() {
        let mut importer = CsvImporter::new(1);

        let csv = "name,description\nAlice,\"Hello, World\"\nBob,\"Say \"\"Hi\"\"\"";
        importer.receive_blob(&encode_csv(csv)).unwrap();

        let data = importer.finish_receive().unwrap();

        assert_eq!(data.rows[0][1], "Hello, World");
        assert_eq!(data.rows[1][1], "Say \"Hi\"");
    }

    #[test]
    fn test_csv_import_generate_mysql_inserts() {
        let mut importer = CsvImporter::new(1);

        let csv = "id,name,value\n1,Alice,100\n2,Bob,200";
        importer.receive_blob(&encode_csv(csv)).unwrap();
        importer.finish_receive().unwrap();

        let inserts = importer.generate_mysql_inserts("users").unwrap();

        assert_eq!(inserts.len(), 2);
        assert!(inserts[0].contains("INSERT INTO `users`"));
        assert!(inserts[0].contains("`id`, `name`, `value`"));
        assert!(inserts[0].contains("1, 'Alice', 100"));
    }

    #[test]
    fn test_csv_import_generate_postgres_inserts() {
        let mut importer = CsvImporter::new(1);

        let csv = "id,name\n1,Alice\n2,Bob";
        importer.receive_blob(&encode_csv(csv)).unwrap();
        importer.finish_receive().unwrap();

        let inserts = importer.generate_postgres_inserts("users").unwrap();

        assert_eq!(inserts.len(), 2);
        assert!(inserts[0].contains("INSERT INTO \"users\""));
        assert!(inserts[0].contains("\"id\", \"name\""));
    }

    #[test]
    fn test_csv_import_generate_sqlserver_inserts() {
        let mut importer = CsvImporter::new(1);

        let csv = "id,name\n1,Alice\n2,Bob";
        importer.receive_blob(&encode_csv(csv)).unwrap();
        importer.finish_receive().unwrap();

        let inserts = importer.generate_sqlserver_inserts("users").unwrap();

        assert_eq!(inserts.len(), 2);
        assert!(inserts[0].contains("INSERT INTO [users]"));
        assert!(inserts[0].contains("[id], [name]"));
    }

    #[test]
    fn test_csv_import_null_handling() {
        let mut importer = CsvImporter::new(1);

        let csv = "id,name,notes\n1,Alice,\n2,,NULL";
        importer.receive_blob(&encode_csv(csv)).unwrap();
        importer.finish_receive().unwrap();

        let inserts = importer.generate_mysql_inserts("users").unwrap();

        // Empty and "NULL" should both become NULL
        assert!(inserts[0].contains("NULL"));
        assert!(inserts[1].contains("NULL"));
    }

    #[test]
    fn test_csv_import_sql_injection_prevention() {
        let mut importer = CsvImporter::new(1);

        // Attempt SQL injection via CSV data
        let csv = "name\n'; DROP TABLE users; --";
        importer.receive_blob(&encode_csv(csv)).unwrap();
        importer.finish_receive().unwrap();

        let inserts = importer.generate_mysql_inserts("users").unwrap();

        // Single quotes should be escaped - the malicious quote becomes ''
        // So the full value is: '''; DROP TABLE users; --'
        // (opening quote, escaped quote, rest of string, closing quote)
        assert!(inserts[0].contains("'''; DROP TABLE users; --'"));

        // The injection attempt is now just a string value, not SQL
        // It should NOT contain the unescaped pattern that would break out
        assert!(!inserts[0].contains("VALUES ('';"));
    }

    #[test]
    fn test_csv_import_cancellation() {
        let importer = CsvImporter::new(1);
        let handle = importer.cancellation_handle();

        assert!(!importer.is_cancelled());

        handle.store(true, std::sync::atomic::Ordering::SeqCst);

        assert!(importer.is_cancelled());
    }

    #[test]
    fn test_csv_import_empty_csv() {
        let mut importer = CsvImporter::new(1);

        let csv = "";
        importer.receive_blob(&encode_csv(csv)).unwrap();

        let result = importer.finish_receive();
        assert!(result.is_err());
    }

    #[test]
    fn test_csv_import_headers_only() {
        let mut importer = CsvImporter::new(1);

        let csv = "id,name,value";
        importer.receive_blob(&encode_csv(csv)).unwrap();

        let data = importer.finish_receive().unwrap();

        assert_eq!(data.headers, vec!["id", "name", "value"]);
        assert_eq!(data.row_count(), 0);
    }
}

mod security_tests {
    use guacr_database::{
        check_query_allowed, classify_query, DatabaseSecuritySettings, QueryType,
    };
    use guacr_handlers::HandlerSecuritySettings;

    #[test]
    fn test_query_classification() {
        assert_eq!(classify_query("SELECT * FROM users"), QueryType::ReadOnly);
        assert_eq!(
            classify_query("INSERT INTO users VALUES (1)"),
            QueryType::Modifying
        );
        assert_eq!(
            classify_query("UPDATE users SET name = 'x'"),
            QueryType::Modifying
        );
        assert_eq!(classify_query("DELETE FROM users"), QueryType::Modifying);
        assert_eq!(classify_query("DROP TABLE users"), QueryType::Modifying);
        assert_eq!(classify_query("SHOW TABLES"), QueryType::ReadOnly);
        assert_eq!(classify_query("DESCRIBE users"), QueryType::ReadOnly);
    }

    #[test]
    fn test_read_only_enforcement() {
        let settings = DatabaseSecuritySettings {
            base: HandlerSecuritySettings {
                read_only: true,
                ..Default::default()
            },
            ..Default::default()
        };

        // Allowed
        assert!(check_query_allowed("SELECT 1", &settings).is_ok());
        assert!(check_query_allowed("SHOW DATABASES", &settings).is_ok());

        // Blocked
        assert!(check_query_allowed("INSERT INTO t VALUES (1)", &settings).is_err());
        assert!(check_query_allowed("UPDATE t SET x = 1", &settings).is_err());
        assert!(check_query_allowed("DELETE FROM t", &settings).is_err());
        assert!(check_query_allowed("DROP TABLE t", &settings).is_err());
        assert!(check_query_allowed("TRUNCATE TABLE t", &settings).is_err());
    }

    #[test]
    fn test_csv_export_disabled() {
        use guacr_database::check_csv_export_allowed;

        let settings = DatabaseSecuritySettings {
            disable_csv_export: true,
            ..Default::default()
        };

        assert!(check_csv_export_allowed(&settings).is_err());

        let settings = DatabaseSecuritySettings::default();
        assert!(check_csv_export_allowed(&settings).is_ok());
    }

    #[test]
    fn test_csv_import_disabled() {
        use guacr_database::check_csv_import_allowed;

        let settings = DatabaseSecuritySettings {
            disable_csv_import: true,
            ..Default::default()
        };

        assert!(check_csv_import_allowed(&settings).is_err());

        let settings = DatabaseSecuritySettings::default();
        assert!(check_csv_import_allowed(&settings).is_ok());
    }

    #[test]
    fn test_csv_import_blocked_in_readonly() {
        use guacr_database::check_csv_import_allowed;

        // Import should be blocked in read-only mode
        // is_csv_import_allowed checks BOTH disable_csv_import AND read_only
        let settings = DatabaseSecuritySettings {
            base: HandlerSecuritySettings {
                read_only: true,
                ..Default::default()
            },
            ..Default::default()
        };

        // Read-only mode blocks import
        assert!(check_csv_import_allowed(&settings).is_err());
    }
}
