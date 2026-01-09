//! URL encoding tests for database connection strings
//!
//! Tests that special characters in passwords are properly URL-encoded
//! to prevent connection failures.
//!
//! Run with: cargo test --package guacr-database --test url_encoding_test

use urlencoding::encode;

/// Test passwords with various special characters that need URL encoding
const TEST_PASSWORDS: &[(&str, &str)] = &[
    // (password, description)
    ("simple123", "alphanumeric only"),
    ("with|pipe", "pipe character"),
    ("with&ampersand", "ampersand"),
    ("with@atsign", "at sign"),
    ("with:colon", "colon"),
    ("with/slash", "forward slash"),
    ("with?question", "question mark"),
    ("with#hash", "hash/pound"),
    ("with%percent", "percent sign"),
    ("with space", "space character"),
    ("with+plus", "plus sign"),
    ("with=equals", "equals sign"),
    ("complex|&@:/#?%", "all special chars"),
    ("p@ssw0rd!", "realistic password"),
    ("my|secure&pass#123", "complex realistic"),
];

#[test]
fn test_url_encode_passwords() {
    println!("\n=== URL Encoding Test Results ===\n");

    for (password, description) in TEST_PASSWORDS {
        let encoded = encode(password);
        let needs_encoding = password != &encoded.to_string();

        println!(
            "{:30} | {:25} | {:30} | {}",
            description,
            password,
            encoded,
            if needs_encoding {
                "ENCODED"
            } else {
                "NO CHANGE"
            }
        );

        // Verify encoding is reversible
        let decoded = urlencoding::decode(&encoded).unwrap();
        assert_eq!(
            decoded, *password,
            "Encoding should be reversible for: {}",
            password
        );
    }

    println!("\n=== All passwords encoded successfully ===\n");
}

#[test]
fn test_mysql_url_construction() {
    let username = "root";
    let hostname = "localhost";
    let port = 3306;
    let database = "testdb";

    for (password, description) in TEST_PASSWORDS {
        let encoded_username = encode(username);
        let encoded_password = encode(password);
        let encoded_database = encode(database);

        let url = format!(
            "mysql://{}:{}@{}:{}/{}",
            encoded_username, encoded_password, hostname, port, encoded_database
        );

        // Verify URL doesn't contain unencoded special chars in password section
        // The password section is between ":" and "@"
        let password_section = url
            .split("://")
            .nth(1)
            .and_then(|s| s.split('@').next())
            .and_then(|s| s.split(':').nth(1));

        if let Some(pwd_in_url) = password_section {
            // Check that special characters are properly encoded
            if password.contains('|') {
                assert!(
                    !pwd_in_url.contains('|'),
                    "Pipe should be encoded in URL for: {}",
                    description
                );
            }
            if password.contains('&') {
                assert!(
                    !pwd_in_url.contains('&'),
                    "Ampersand should be encoded in URL for: {}",
                    description
                );
            }
            if password.contains('@') {
                assert!(
                    !pwd_in_url.contains('@'),
                    "At sign should be encoded in URL for: {}",
                    description
                );
            }
        }

        println!("✓ MySQL URL valid for: {}", description);
    }
}

#[test]
fn test_postgres_url_construction() {
    let username = "testuser";
    let hostname = "localhost";
    let port = 5432;
    let database = "testdb";

    for (password, description) in TEST_PASSWORDS {
        let encoded_username = encode(username);
        let encoded_password = encode(password);
        let encoded_database = encode(database);

        let url = format!(
            "postgres://{}:{}@{}:{}/{}",
            encoded_username, encoded_password, hostname, port, encoded_database
        );

        // Verify URL is well-formed
        assert!(
            url.starts_with("postgres://"),
            "URL should start with postgres://"
        );
        assert!(
            url.contains(&encoded_password.to_string()),
            "URL should contain encoded password"
        );

        println!("✓ PostgreSQL URL valid for: {}", description);
    }
}

#[test]
fn test_mongodb_url_construction() {
    let username = "testuser";
    let hostname = "localhost";
    let port = 27017;
    let database = "testdb";
    let auth_source = "admin";

    for (password, description) in TEST_PASSWORDS {
        let encoded_username = encode(username);
        let encoded_password = encode(password);
        let encoded_database = encode(database);
        let encoded_auth_source = encode(auth_source);

        let url = format!(
            "mongodb://{}:{}@{}:{}/{}?authSource={}",
            encoded_username,
            encoded_password,
            hostname,
            port,
            encoded_database,
            encoded_auth_source
        );

        // Verify URL is well-formed
        assert!(
            url.starts_with("mongodb://"),
            "URL should start with mongodb://"
        );
        assert!(
            url.contains("authSource="),
            "URL should contain authSource parameter"
        );

        println!("✓ MongoDB URL valid for: {}", description);
    }
}

#[test]
fn test_redis_url_construction() {
    let hostname = "localhost";
    let port = 6379;
    let database = 0;

    for (password, description) in TEST_PASSWORDS {
        let encoded_password = encode(password);

        let url = format!(
            "redis://:{}@{}:{}/{}",
            encoded_password, hostname, port, database
        );

        // Verify URL is well-formed
        assert!(
            url.starts_with("redis://"),
            "URL should start with redis://"
        );
        assert!(
            url.contains(&encoded_password.to_string()),
            "URL should contain encoded password"
        );

        println!("✓ Redis URL valid for: {}", description);
    }
}

#[test]
fn test_special_char_encoding_correctness() {
    // Test specific character encodings
    assert_eq!(encode("|").to_string(), "%7C");
    assert_eq!(encode("&").to_string(), "%26");
    assert_eq!(encode("@").to_string(), "%40");
    assert_eq!(encode(":").to_string(), "%3A");
    assert_eq!(encode("/").to_string(), "%2F");
    assert_eq!(encode("?").to_string(), "%3F");
    assert_eq!(encode("#").to_string(), "%23");
    assert_eq!(encode("%").to_string(), "%25");
    assert_eq!(encode(" ").to_string(), "%20");
    assert_eq!(encode("+").to_string(), "%2B");
    assert_eq!(encode("=").to_string(), "%3D");

    println!("✓ All special characters encoded correctly");
}

#[test]
fn test_username_encoding() {
    // Usernames can also contain special characters
    let special_usernames = vec!["user@domain.com", "user:name", "user/name", "user name"];

    for username in special_usernames {
        let encoded = encode(username);
        println!("Username '{}' -> '{}'", username, encoded);

        // Verify encoding is reversible
        let decoded = urlencoding::decode(&encoded).unwrap();
        assert_eq!(decoded, username);
    }
}

#[test]
fn test_database_name_encoding() {
    // Database names might contain special characters
    let special_databases = vec!["test-db", "test_db", "test.db", "test db"];

    for database in special_databases {
        let encoded = encode(database);
        println!("Database '{}' -> '{}'", database, encoded);

        // Verify encoding is reversible
        let decoded = urlencoding::decode(&encoded).unwrap();
        assert_eq!(decoded, database);
    }
}

#[test]
fn test_real_world_scenarios() {
    // Test realistic password scenarios that have caused issues

    // Scenario 1: Password from bug report
    let password = "my|password";
    let encoded = encode(password);
    assert_eq!(encoded.to_string(), "my%7Cpassword");
    println!(
        "✓ Bug report password encoded correctly: {} -> {}",
        password, encoded
    );

    // Scenario 2: Generated password with special chars
    let password = "Xy7$mK9@pL2!";
    let encoded = encode(password);
    assert!(!encoded.contains('@'));
    assert!(!encoded.contains('$'));
    assert!(!encoded.contains('!'));
    println!("✓ Generated password encoded correctly");

    // Scenario 3: Password with multiple problematic chars
    let password = "pass&word|123@host";
    let encoded = encode(password);
    assert!(!encoded.contains('&'));
    assert!(!encoded.contains('|'));
    assert!(!encoded.contains('@'));
    println!("✓ Complex password encoded correctly");
}

#[test]
fn test_url_parsing_with_encoded_passwords() {
    // Verify that mysql_async can parse URLs with encoded passwords
    // This doesn't actually connect, just tests URL parsing

    let test_cases = vec![
        ("simple", "mysql://root:simple@localhost:3306/db"),
        ("with%7Cpipe", "mysql://root:with%7Cpipe@localhost:3306/db"),
        ("with%26amp", "mysql://root:with%26amp@localhost:3306/db"),
        ("with%40at", "mysql://root:with%40at@localhost:3306/db"),
    ];

    for (description, url) in test_cases {
        match mysql_async::Opts::from_url(url) {
            Ok(_) => println!("✓ MySQL URL parsed successfully: {}", description),
            Err(e) => panic!("Failed to parse URL for {}: {}", description, e),
        }
    }
}

#[test]
fn test_edge_cases() {
    // Test edge cases

    // Empty password
    let encoded = encode("");
    assert_eq!(encoded.to_string(), "");

    // Password is just special characters
    let encoded = encode("!@#$%^&*()");
    assert!(!encoded.contains('@'));
    assert!(!encoded.contains('&'));

    // Unicode characters
    let password = "пароль"; // Russian for "password"
    let encoded = encode(password);
    let decoded = urlencoding::decode(&encoded).unwrap();
    assert_eq!(decoded, password);

    // Emoji in password (why not?)
    let password = "pass🔒word";
    let encoded = encode(password);
    let decoded = urlencoding::decode(&encoded).unwrap();
    assert_eq!(decoded, password);

    println!("✓ All edge cases handled correctly");
}

#[test]
fn test_security_no_password_leakage() {
    // Ensure that debug output doesn't leak passwords

    let password = "secret|password";
    let encoded = encode(password);

    let url = format!("mysql://root:{}@localhost:3306/db", encoded);

    // Simulate what the debug log should do
    let safe_log = url.replace(&encoded.to_string(), "***");

    assert!(!safe_log.contains("secret"));
    assert!(!safe_log.contains("password"));
    assert!(safe_log.contains("***"));

    println!("✓ Password masking works correctly");
}
