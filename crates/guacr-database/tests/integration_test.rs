//! Integration tests for database handlers
//!
//! These tests require running databases. Start them with:
//!   docker-compose -f docker-compose.test.yml up -d
//!
//! Run tests with:
//!   cargo test --package guacr-database --test integration_test
//!
//! Connection details:
//!   MySQL:          localhost:13306, testuser/testpassword, testdb
//!   MariaDB:        localhost:13307, testuser/testpassword, testdb
//!   PostgreSQL:     localhost:15432, testuser/testpassword, testdb
//!   SQL Server:     localhost:11433, sa/TestPassword123!
//!   MongoDB:        localhost:17017, testuser/testpassword, testdb
//!   Redis:          localhost:16379, password: testpassword
//!   Elasticsearch:  localhost:19200 (no auth for test container)
//!   Cassandra:      localhost:19042 (no auth for test container)
//!   DynamoDB:       localhost:18000 (DynamoDB Local, no auth required)
//!   ODBC:           localhost:15433, testuser/testpassword, testdb (requires psqlodbc driver)

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

mod elasticsearch_tests {
    use super::*;

    const HOST: &str = "127.0.0.1";
    const PORT: u16 = 19200;

    async fn skip_if_not_available() -> bool {
        if !port_is_open(HOST, PORT).await {
            eprintln!(
                "Skipping Elasticsearch tests - database not available on {}:{}",
                HOST, PORT
            );
            return true;
        }
        false
    }

    /// Build a reqwest client for testing
    fn build_test_client() -> reqwest::Client {
        reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client")
    }

    /// Wait for Elasticsearch to be fully ready (not just port open)
    async fn wait_for_ready(client: &reqwest::Client) -> bool {
        let url = format!("http://{}:{}/_cluster/health", HOST, PORT);
        for _ in 0..10 {
            if let Ok(resp) = client.get(&url).send().await {
                if resp.status().is_success() {
                    return true;
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        false
    }

    /// Clean up a test index (best effort)
    async fn cleanup_index(client: &reqwest::Client, index: &str) {
        let url = format!("http://{}:{}/{}", HOST, PORT, index);
        let _ = client.delete(&url).send().await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    #[tokio::test]
    async fn test_elasticsearch_connection() {
        if skip_if_not_available().await {
            return;
        }

        let client = build_test_client();
        if !wait_for_ready(&client).await {
            eprintln!("Skipping - Elasticsearch not ready");
            return;
        }

        let url = format!("http://{}:{}/_cluster/health", HOST, PORT);
        let result = timeout(CONNECT_TIMEOUT, client.get(&url).send()).await;
        assert!(result.is_ok(), "Connection timed out");

        let resp = result.unwrap();
        assert!(resp.is_ok(), "Failed to connect to Elasticsearch");

        let resp = resp.unwrap();
        assert!(resp.status().is_success(), "Health check failed");

        let body: serde_json::Value = resp.json().await.expect("Invalid JSON");
        assert!(
            body.get("cluster_name").is_some(),
            "Expected cluster_name in health response"
        );
        let status = body["status"].as_str().unwrap_or("unknown");
        println!(
            "Elasticsearch: cluster={}, status={}",
            body["cluster_name"].as_str().unwrap_or("?"),
            status
        );
    }

    #[tokio::test]
    async fn test_elasticsearch_sql_query() {
        if skip_if_not_available().await {
            return;
        }

        let client = build_test_client();
        if !wait_for_ready(&client).await {
            eprintln!("Skipping - Elasticsearch not ready");
            return;
        }

        let index = "guacr_test_sql";
        cleanup_index(&client, index).await;

        // Create index and index some documents
        let doc1 = serde_json::json!({"name": "Alice", "age": 30, "city": "NYC"});
        let doc2 = serde_json::json!({"name": "Bob", "age": 25, "city": "LA"});
        let doc3 = serde_json::json!({"name": "Charlie", "age": 35, "city": "Chicago"});

        let base = format!("http://{}:{}", HOST, PORT);

        // Index documents
        for (i, doc) in [doc1, doc2, doc3].iter().enumerate() {
            let url = format!("{}/{}/_doc/{}", base, index, i + 1);
            let resp = client
                .put(&url)
                .header("Content-Type", "application/json")
                .json(doc)
                .send()
                .await
                .expect("Failed to index document");
            assert!(
                resp.status().is_success() || resp.status().as_u16() == 201,
                "Index document {} failed: {}",
                i + 1,
                resp.status()
            );
        }

        // Refresh the index to make documents searchable
        let refresh_url = format!("{}/{}/_refresh", base, index);
        client
            .post(&refresh_url)
            .send()
            .await
            .expect("Failed to refresh index");

        // Execute SQL query
        let sql_url = format!("{}/_sql?format=json", base);
        let sql_body = serde_json::json!({
            "query": format!("SELECT name, age, city FROM {} ORDER BY age", index)
        });

        let resp = client
            .post(&sql_url)
            .header("Content-Type", "application/json")
            .json(&sql_body)
            .send()
            .await
            .expect("SQL query failed");

        assert!(
            resp.status().is_success(),
            "SQL query returned error: {}",
            resp.status()
        );

        let body: serde_json::Value = resp.json().await.expect("Invalid JSON");

        // Verify columns
        let columns = body["columns"].as_array().expect("Expected columns array");
        assert_eq!(columns.len(), 3);
        assert_eq!(columns[0]["name"].as_str().unwrap(), "name");

        // Verify rows
        let rows = body["rows"].as_array().expect("Expected rows array");
        assert_eq!(rows.len(), 3, "Expected 3 rows, got {}", rows.len());
        println!("Elasticsearch SQL: Found {} rows", rows.len());

        // Clean up
        cleanup_index(&client, index).await;
    }

    #[tokio::test]
    async fn test_elasticsearch_json_query() {
        if skip_if_not_available().await {
            return;
        }

        let client = build_test_client();
        if !wait_for_ready(&client).await {
            eprintln!("Skipping - Elasticsearch not ready");
            return;
        }

        let index = "guacr_test_json";
        cleanup_index(&client, index).await;

        let base = format!("http://{}:{}", HOST, PORT);

        // Index some documents
        let doc1 = serde_json::json!({"product": "Widget", "price": 9.99});
        let doc2 = serde_json::json!({"product": "Gadget", "price": 19.99});

        for (i, doc) in [doc1, doc2].iter().enumerate() {
            let url = format!("{}/{}/_doc/{}", base, index, i + 1);
            client
                .put(&url)
                .header("Content-Type", "application/json")
                .json(doc)
                .send()
                .await
                .expect("Failed to index document");
        }

        // Refresh
        let refresh_url = format!("{}/{}/_refresh", base, index);
        client
            .post(&refresh_url)
            .send()
            .await
            .expect("Refresh failed");

        // JSON search query
        let search_url = format!("{}/{}/_search", base, index);
        let search_body = serde_json::json!({
            "query": {
                "match": {
                    "product": "Widget"
                }
            }
        });

        let resp = client
            .post(&search_url)
            .header("Content-Type", "application/json")
            .json(&search_body)
            .send()
            .await
            .expect("Search query failed");

        assert!(resp.status().is_success());

        let body: serde_json::Value = resp.json().await.expect("Invalid JSON");
        let hits = body["hits"]["hits"]
            .as_array()
            .expect("Expected hits array");
        assert_eq!(hits.len(), 1, "Expected 1 hit for 'Widget'");
        assert_eq!(hits[0]["_source"]["product"].as_str().unwrap(), "Widget");

        println!("Elasticsearch JSON search: Found {} hits", hits.len());

        // Clean up
        cleanup_index(&client, index).await;
    }

    #[tokio::test]
    async fn test_elasticsearch_cat_indices() {
        if skip_if_not_available().await {
            return;
        }

        let client = build_test_client();
        if !wait_for_ready(&client).await {
            eprintln!("Skipping - Elasticsearch not ready");
            return;
        }

        let index = "guacr_test_cat";
        let base = format!("http://{}:{}", HOST, PORT);

        // Create an index
        let create_url = format!("{}/{}", base, index);
        client
            .put(&create_url)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({}))
            .send()
            .await
            .expect("Failed to create index");

        tokio::time::sleep(Duration::from_secs(1)).await;

        // List indices
        let url = format!("{}/_cat/indices?format=json", base);
        let resp = client
            .get(&url)
            .send()
            .await
            .expect("cat/indices request failed");

        assert!(resp.status().is_success());

        let body: serde_json::Value = resp.json().await.expect("Invalid JSON");
        let indices = body.as_array().expect("Expected array");

        let has_test_index = indices.iter().any(|idx| {
            idx.get("index")
                .and_then(|v| v.as_str())
                .map(|s| s == index)
                .unwrap_or(false)
        });
        assert!(
            has_test_index,
            "Expected to find '{}' in indices list",
            index
        );

        println!("Elasticsearch cat/indices: Found {} indices", indices.len());

        // Clean up
        cleanup_index(&client, index).await;
    }
}

mod dynamodb_tests {
    use super::*;

    const HOST: &str = "127.0.0.1";
    const PORT: u16 = 18000;

    async fn skip_if_not_available() -> bool {
        if !port_is_open(HOST, PORT).await {
            eprintln!(
                "Skipping DynamoDB tests - database not available on {}:{}",
                HOST, PORT
            );
            return true;
        }
        false
    }

    /// Build a DynamoDB client for testing against DynamoDB Local.
    async fn build_test_client() -> aws_sdk_dynamodb::Client {
        use aws_sdk_dynamodb::config::Credentials;
        use aws_types::region::Region;

        let endpoint_url = format!("http://{}:{}", HOST, PORT);
        let creds = Credentials::new("fakeAccessKey", "fakeSecretKey", None, None, "test");
        let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(Region::new("us-east-1"))
            .credentials_provider(creds)
            .endpoint_url(&endpoint_url)
            .load()
            .await;
        aws_sdk_dynamodb::Client::new(&sdk_config)
    }

    /// Clean up a test table (best effort).
    async fn cleanup_table(client: &aws_sdk_dynamodb::Client, table_name: &str) {
        let _ = client.delete_table().table_name(table_name).send().await;
        // Wait briefly for deletion to propagate
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    #[tokio::test]
    async fn test_dynamodb_connection() {
        if skip_if_not_available().await {
            return;
        }

        let client = build_test_client().await;

        // Verify we can list tables (proves connectivity)
        let result = timeout(CONNECT_TIMEOUT, client.list_tables().send()).await;
        assert!(result.is_ok(), "Connection timed out");
        assert!(
            result.unwrap().is_ok(),
            "Failed to connect to DynamoDB Local"
        );
    }

    #[tokio::test]
    async fn test_dynamodb_create_and_query() {
        if skip_if_not_available().await {
            return;
        }

        use aws_sdk_dynamodb::types::{
            AttributeDefinition, AttributeValue, BillingMode, KeySchemaElement, KeyType,
            ScalarAttributeType,
        };

        let client = build_test_client().await;
        let table_name = "guacr_test_items";

        // Clean up any leftover table from previous test run
        cleanup_table(&client, table_name).await;

        // Create table
        let create_result = client
            .create_table()
            .table_name(table_name)
            .key_schema(
                KeySchemaElement::builder()
                    .attribute_name("id")
                    .key_type(KeyType::Hash)
                    .build()
                    .unwrap(),
            )
            .attribute_definitions(
                AttributeDefinition::builder()
                    .attribute_name("id")
                    .attribute_type(ScalarAttributeType::S)
                    .build()
                    .unwrap(),
            )
            .billing_mode(BillingMode::PayPerRequest)
            .send()
            .await;

        assert!(
            create_result.is_ok(),
            "Failed to create table: {:?}",
            create_result.err()
        );

        // Wait for table to become active
        tokio::time::sleep(Duration::from_secs(1)).await;

        // Put items
        for i in 1..=3 {
            let put_result = client
                .put_item()
                .table_name(table_name)
                .item("id", AttributeValue::S(format!("item_{}", i)))
                .item("name", AttributeValue::S(format!("Item {}", i)))
                .item("count", AttributeValue::N(format!("{}", i * 10)))
                .send()
                .await;
            assert!(
                put_result.is_ok(),
                "Failed to put item {}: {:?}",
                i,
                put_result.err()
            );
        }

        // Query via PartiQL
        let query_result = client
            .execute_statement()
            .statement(format!("SELECT * FROM \"{}\"", table_name))
            .send()
            .await;

        assert!(
            query_result.is_ok(),
            "PartiQL query failed: {:?}",
            query_result.err()
        );
        let items = query_result.unwrap().items().to_vec();
        assert_eq!(items.len(), 3, "Expected 3 items, got {}", items.len());

        // Verify item content
        let has_item_1 = items.iter().any(|item| {
            item.get("id")
                .and_then(|v| v.as_s().ok())
                .map(|s| s == "item_1")
                .unwrap_or(false)
        });
        assert!(has_item_1, "Expected to find item_1 in results");

        // Clean up
        cleanup_table(&client, table_name).await;
    }

    #[tokio::test]
    async fn test_dynamodb_list_tables() {
        if skip_if_not_available().await {
            return;
        }

        use aws_sdk_dynamodb::types::{
            AttributeDefinition, BillingMode, KeySchemaElement, KeyType, ScalarAttributeType,
        };

        let client = build_test_client().await;
        let table_name = "guacr_test_list";

        // Clean up any leftover table
        cleanup_table(&client, table_name).await;

        // Create a table so we have something to list
        let _ = client
            .create_table()
            .table_name(table_name)
            .key_schema(
                KeySchemaElement::builder()
                    .attribute_name("pk")
                    .key_type(KeyType::Hash)
                    .build()
                    .unwrap(),
            )
            .attribute_definitions(
                AttributeDefinition::builder()
                    .attribute_name("pk")
                    .attribute_type(ScalarAttributeType::S)
                    .build()
                    .unwrap(),
            )
            .billing_mode(BillingMode::PayPerRequest)
            .send()
            .await;

        tokio::time::sleep(Duration::from_secs(1)).await;

        // List tables
        let list_result = client.list_tables().send().await;
        assert!(
            list_result.is_ok(),
            "ListTables failed: {:?}",
            list_result.err()
        );

        let table_names = list_result.unwrap().table_names().to_vec();
        assert!(
            table_names.contains(&table_name.to_string()),
            "Expected table '{}' in list: {:?}",
            table_name,
            table_names
        );

        // Clean up
        cleanup_table(&client, table_name).await;
    }

    #[tokio::test]
    async fn test_dynamodb_read_only() {
        use guacr_database::DatabaseSecuritySettings;
        use guacr_handlers::HandlerSecuritySettings;

        // Test that our read-only classification works for DynamoDB statements
        let settings = DatabaseSecuritySettings {
            base: HandlerSecuritySettings {
                read_only: true,
                ..Default::default()
            },
            ..Default::default()
        };

        // The security module's classify_query works for SQL-like statements.
        // For DynamoDB, the handler uses is_dynamodb_modifying_statement internally.
        // Here we verify the security settings struct can be created and read_only is set.
        assert!(settings.base.read_only);

        // Verify that the general query classifier recognizes DynamoDB-like patterns
        use guacr_database::classify_query;

        // SELECT is read-only
        assert_eq!(
            classify_query("SELECT * FROM \"Users\""),
            guacr_database::QueryType::ReadOnly
        );

        // INSERT is modifying
        assert_eq!(
            classify_query("INSERT INTO \"Users\" VALUE {'id': '1'}"),
            guacr_database::QueryType::Modifying
        );

        // UPDATE is modifying
        assert_eq!(
            classify_query("UPDATE \"Users\" SET name = 'x' WHERE id = '1'"),
            guacr_database::QueryType::Modifying
        );

        // DELETE is modifying
        assert_eq!(
            classify_query("DELETE FROM \"Users\" WHERE id = '1'"),
            guacr_database::QueryType::Modifying
        );
    }
}

mod cassandra_tests {
    use super::*;

    const HOST: &str = "127.0.0.1";
    const PORT: u16 = 19042;

    async fn skip_if_not_available() -> bool {
        if !port_is_open(HOST, PORT).await {
            eprintln!(
                "Skipping Cassandra tests - database not available on {}:{}",
                HOST, PORT
            );
            return true;
        }
        false
    }

    #[tokio::test]
    async fn test_cassandra_connection() {
        if skip_if_not_available().await {
            return;
        }

        let session = scylla::SessionBuilder::new()
            .known_node(format!("{}:{}", HOST, PORT))
            .build()
            .await;

        assert!(
            session.is_ok(),
            "Failed to connect to Cassandra: {:?}",
            session.err()
        );
    }

    #[tokio::test]
    async fn test_cassandra_query() {
        if skip_if_not_available().await {
            return;
        }

        let session = scylla::SessionBuilder::new()
            .known_node(format!("{}:{}", HOST, PORT))
            .build()
            .await
            .expect("Failed to connect");

        // Create a test keyspace
        session
            .query_unpaged(
                "CREATE KEYSPACE IF NOT EXISTS guacr_test \
                 WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1}",
                &[],
            )
            .await
            .expect("Failed to create keyspace");

        // Create a test table
        session
            .query_unpaged(
                "CREATE TABLE IF NOT EXISTS guacr_test.users \
                 (id int PRIMARY KEY, name text, age int)",
                &[],
            )
            .await
            .expect("Failed to create table");

        // Insert test data
        session
            .query_unpaged(
                "INSERT INTO guacr_test.users (id, name, age) VALUES (1, 'Alice', 30)",
                &[],
            )
            .await
            .expect("Failed to insert");

        session
            .query_unpaged(
                "INSERT INTO guacr_test.users (id, name, age) VALUES (2, 'Bob', 25)",
                &[],
            )
            .await
            .expect("Failed to insert");

        // Query the data
        let result = session
            .query_unpaged("SELECT id, name, age FROM guacr_test.users", &[])
            .await
            .expect("SELECT failed");

        let mut count = 0;
        let rows_iter = result
            .rows_typed::<(i32, String, i32)>()
            .expect("Expected rows");
        for row in rows_iter {
            let (id, name, age) = row.expect("Row error");
            println!("Cassandra: id={}, name={}, age={}", id, name, age);
            count += 1;
        }

        assert!(count >= 2, "Expected at least 2 users, got {}", count);

        // Clean up
        let _ = session
            .query_unpaged("DROP KEYSPACE IF EXISTS guacr_test", &[])
            .await;
    }

    #[tokio::test]
    async fn test_cassandra_read_only() {
        // Verify that our CQL modifying command detection works correctly
        // (this test does not require a running Cassandra instance)

        use guacr_database::DatabaseSecuritySettings;
        use guacr_handlers::HandlerSecuritySettings;

        let settings = DatabaseSecuritySettings {
            base: HandlerSecuritySettings {
                read_only: true,
                ..Default::default()
            },
            ..Default::default()
        };

        // The general SQL classifier also catches CQL mutations since CQL
        // uses the same keywords (INSERT, UPDATE, DELETE, DROP, etc.)
        use guacr_database::{check_query_allowed, classify_query, QueryType};

        // SELECT is read-only
        assert_eq!(classify_query("SELECT * FROM users"), QueryType::ReadOnly);

        // INSERT is modifying
        assert_eq!(
            classify_query("INSERT INTO users (id, name) VALUES (1, 'test')"),
            QueryType::Modifying
        );

        // UPDATE is modifying
        assert_eq!(
            classify_query("UPDATE users SET name = 'x' WHERE id = 1"),
            QueryType::Modifying
        );

        // DELETE is modifying
        assert_eq!(
            classify_query("DELETE FROM users WHERE id = 1"),
            QueryType::Modifying
        );

        // DROP is modifying
        assert_eq!(classify_query("DROP TABLE users"), QueryType::Modifying);

        // TRUNCATE is modifying
        assert_eq!(classify_query("TRUNCATE users"), QueryType::Modifying);

        // CREATE is modifying
        assert_eq!(
            classify_query("CREATE TABLE t (id int PRIMARY KEY)"),
            QueryType::Modifying
        );

        // Read-only enforcement: SELECT allowed, INSERT blocked
        assert!(check_query_allowed("SELECT * FROM users", &settings).is_ok());
        assert!(check_query_allowed("INSERT INTO users (id) VALUES (1)", &settings).is_err());
    }
}

mod odbc_tests {
    use super::*;
    use odbc_api::ResultSetMetadata;

    const HOST: &str = "127.0.0.1";
    const PORT: u16 = 15433;

    async fn skip_if_not_available() -> bool {
        if !port_is_open(HOST, PORT).await {
            eprintln!(
                "Skipping ODBC tests - PostgreSQL not available on {}:{}",
                HOST, PORT
            );
            return true;
        }
        false
    }

    /// Check if the PostgreSQL ODBC driver is installed on this machine.
    /// Returns the driver name if found, or None if not installed.
    fn find_odbc_driver() -> Option<String> {
        use odbc_api::Environment;

        let env = match Environment::new() {
            Ok(env) => env,
            Err(_) => return None,
        };

        let drivers = match env.drivers() {
            Ok(d) => d,
            Err(_) => return None,
        };

        // Look for a PostgreSQL ODBC driver (common names across platforms)
        for driver_info in &drivers {
            let desc = driver_info.description.to_lowercase();
            if desc.contains("postgres") || desc.contains("psql") {
                return Some(driver_info.description.clone());
            }
        }

        None
    }

    /// Skip if the PostgreSQL ODBC driver is not installed.
    fn skip_if_no_driver() -> Option<String> {
        match find_odbc_driver() {
            Some(driver) => Some(driver),
            None => {
                eprintln!(
                    "Skipping ODBC tests - no PostgreSQL ODBC driver installed. \
                     Install with: brew install psqlodbc (macOS) or \
                     apt-get install odbc-postgresql (Ubuntu)"
                );
                None
            }
        }
    }

    #[tokio::test]
    async fn test_odbc_connection() {
        if skip_if_not_available().await {
            return;
        }
        let driver = match skip_if_no_driver() {
            Some(d) => d,
            None => return,
        };

        let conn_str = format!(
            "DRIVER={{{}}};SERVER={};PORT={};DATABASE=testdb;UID=testuser;PWD=testpassword",
            driver, HOST, PORT
        );

        let result: Result<(), String> = tokio::task::spawn_blocking(move || {
            use odbc_api::{ConnectionOptions, Environment};

            let env = Environment::new().map_err(|e| format!("Environment error: {}", e))?;

            let _conn = env
                .connect_with_connection_string(&conn_str, ConnectionOptions::default())
                .map_err(|e| format!("Connection error: {}", e))?;

            Ok(())
        })
        .await
        .map_err(|e| format!("Task error: {}", e))
        .and_then(|r| r);

        assert!(result.is_ok(), "ODBC connection failed: {:?}", result.err());
    }

    #[tokio::test]
    async fn test_odbc_query() {
        if skip_if_not_available().await {
            return;
        }
        let driver = match skip_if_no_driver() {
            Some(d) => d,
            None => return,
        };

        let conn_str = format!(
            "DRIVER={{{}}};SERVER={};PORT={};DATABASE=testdb;UID=testuser;PWD=testpassword",
            driver, HOST, PORT
        );

        let result: Result<(Vec<String>, Vec<Vec<String>>), String> =
            tokio::task::spawn_blocking(move || {
                use odbc_api::buffers::TextRowSet;
                use odbc_api::{ConnectionOptions, Cursor, Environment};

                let env = Environment::new().map_err(|e| format!("Environment error: {}", e))?;

                let conn = env
                    .connect_with_connection_string(&conn_str, ConnectionOptions::default())
                    .map_err(|e| format!("Connection error: {}", e))?;

                // Create a test table and insert data
                conn.execute(
                    "CREATE TABLE IF NOT EXISTS odbc_test (id INT, name VARCHAR(100))",
                    (),
                    None,
                )
                .map_err(|e| format!("CREATE TABLE error: {}", e))?;

                conn.execute("DELETE FROM odbc_test", (), None)
                    .map_err(|e| format!("DELETE error: {}", e))?;

                conn.execute(
                    "INSERT INTO odbc_test (id, name) VALUES (1, 'Alice')",
                    (),
                    None,
                )
                .map_err(|e| format!("INSERT error: {}", e))?;

                conn.execute(
                    "INSERT INTO odbc_test (id, name) VALUES (2, 'Bob')",
                    (),
                    None,
                )
                .map_err(|e| format!("INSERT error: {}", e))?;

                // Query the data back
                let mut cursor = conn
                    .execute("SELECT id, name FROM odbc_test ORDER BY id", (), None)
                    .map_err(|e| format!("SELECT error: {}", e))?
                    .ok_or_else(|| "Expected cursor for SELECT".to_string())?;

                let num_cols = cursor
                    .num_result_cols()
                    .map_err(|e| format!("Column count error: {}", e))?
                    as u16;

                let mut columns = Vec::new();
                for i in 1..=num_cols {
                    columns.push(
                        cursor
                            .col_name(i)
                            .map_err(|e| format!("Col name error: {}", e))?,
                    );
                }

                let buffer = TextRowSet::for_cursor(100, &mut cursor, Some(4096))
                    .map_err(|e| format!("Buffer error: {}", e))?;

                let mut row_cursor = cursor
                    .bind_buffer(buffer)
                    .map_err(|e| format!("Bind error: {}", e))?;

                let mut rows = Vec::new();
                while let Some(batch) = row_cursor
                    .fetch()
                    .map_err(|e| format!("Fetch error: {}", e))?
                {
                    for row_idx in 0..batch.num_rows() {
                        let mut row = Vec::new();
                        for col_idx in 0..(num_cols as usize) {
                            let val = batch
                                .at(col_idx, row_idx)
                                .map(|b| String::from_utf8_lossy(b).to_string())
                                .unwrap_or_else(|| "NULL".to_string());
                            row.push(val);
                        }
                        rows.push(row);
                    }
                }

                // Clean up
                conn.execute("DROP TABLE IF EXISTS odbc_test", (), None)
                    .map_err(|e| format!("DROP TABLE error: {}", e))?;

                Ok((columns, rows))
            })
            .await
            .map_err(|e| format!("Task error: {}", e))
            .and_then(|r| r);

        match result {
            Ok((columns, rows)) => {
                assert_eq!(columns.len(), 2);
                assert_eq!(rows.len(), 2, "Expected 2 rows");
                assert_eq!(rows[0][1], "Alice");
                assert_eq!(rows[1][1], "Bob");
                println!(
                    "ODBC: Query returned {} rows with {} columns",
                    rows.len(),
                    columns.len()
                );
            }
            Err(e) => panic!("ODBC query test failed: {}", e),
        }
    }

    #[tokio::test]
    async fn test_odbc_list_tables() {
        if skip_if_not_available().await {
            return;
        }
        let driver = match skip_if_no_driver() {
            Some(d) => d,
            None => return,
        };

        let conn_str = format!(
            "DRIVER={{{}}};SERVER={};PORT={};DATABASE=testdb;UID=testuser;PWD=testpassword",
            driver, HOST, PORT
        );

        let result: Result<Vec<String>, String> = tokio::task::spawn_blocking(move || {
            use odbc_api::buffers::TextRowSet;
            use odbc_api::{ConnectionOptions, Cursor, Environment};

            let env = Environment::new().map_err(|e| format!("Environment error: {}", e))?;

            let conn = env
                .connect_with_connection_string(&conn_str, ConnectionOptions::default())
                .map_err(|e| format!("Connection error: {}", e))?;

            // Create a test table to be sure there is at least one
            conn.execute(
                "CREATE TABLE IF NOT EXISTS odbc_tables_test (id INT)",
                (),
                None,
            )
            .map_err(|e| format!("CREATE TABLE error: {}", e))?;

            // Use SQLTables catalog function
            let mut cursor = conn
                .tables("", "", "", "TABLE")
                .map_err(|e| format!("SQLTables error: {}", e))?;

            let buffer = TextRowSet::for_cursor(1000, &mut cursor, Some(4096))
                .map_err(|e| format!("Buffer error: {}", e))?;

            let mut row_cursor = cursor
                .bind_buffer(buffer)
                .map_err(|e| format!("Bind error: {}", e))?;

            let mut table_names = Vec::new();
            while let Some(batch) = row_cursor
                .fetch()
                .map_err(|e| format!("Fetch error: {}", e))?
            {
                for row_idx in 0..batch.num_rows() {
                    // TABLE_NAME is column index 2 (0-based)
                    if let Some(name_bytes) = batch.at(2, row_idx) {
                        table_names.push(String::from_utf8_lossy(name_bytes).to_string());
                    }
                }
            }

            // Clean up
            conn.execute("DROP TABLE IF EXISTS odbc_tables_test", (), None)
                .map_err(|e| format!("DROP error: {}", e))?;

            Ok(table_names)
        })
        .await
        .map_err(|e| format!("Task error: {}", e))
        .and_then(|r| r);

        match result {
            Ok(tables) => {
                assert!(
                    !tables.is_empty(),
                    "Expected at least one table from SQLTables"
                );
                println!("ODBC: SQLTables returned {} tables", tables.len());
            }
            Err(e) => panic!("ODBC list tables test failed: {}", e),
        }
    }

    #[tokio::test]
    async fn test_odbc_read_only() {
        // Verify that security settings work for ODBC (same shared module as other handlers)
        use guacr_database::{
            check_query_allowed, classify_query, DatabaseSecuritySettings, QueryType,
        };
        use guacr_handlers::HandlerSecuritySettings;

        let settings = DatabaseSecuritySettings {
            base: HandlerSecuritySettings {
                read_only: true,
                ..Default::default()
            },
            ..Default::default()
        };

        // SELECT should be allowed
        assert_eq!(classify_query("SELECT * FROM users"), QueryType::ReadOnly);
        assert!(check_query_allowed("SELECT * FROM users", &settings).is_ok());

        // INSERT should be blocked
        assert_eq!(
            classify_query("INSERT INTO t VALUES (1)"),
            QueryType::Modifying
        );
        assert!(check_query_allowed("INSERT INTO t VALUES (1)", &settings).is_err());

        // UPDATE should be blocked
        assert!(check_query_allowed("UPDATE t SET x = 1", &settings).is_err());

        // DELETE should be blocked
        assert!(check_query_allowed("DELETE FROM t", &settings).is_err());

        // DROP should be blocked
        assert!(check_query_allowed("DROP TABLE t", &settings).is_err());

        // TRUNCATE should be blocked
        assert!(check_query_allowed("TRUNCATE TABLE t", &settings).is_err());
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
