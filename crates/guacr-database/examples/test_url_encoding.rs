//! Example demonstrating URL encoding for database passwords
//!
//! Run with: cargo run --example test_url_encoding

use urlencoding::encode;

fn main() {
    println!("\n=== Database Password URL Encoding Demo ===\n");

    // Test passwords that previously caused failures
    let test_passwords = vec![
        ("simple123", "Safe password (no encoding needed)"),
        ("my|password", "Password with pipe (BUG REPORT)"),
        ("pass&word", "Password with ampersand"),
        ("p@ssw0rd", "Password with at sign"),
        ("user:pass", "Password with colon"),
        ("with/slash", "Password with slash"),
        ("test?query", "Password with question mark"),
        ("hash#tag", "Password with hash"),
        ("50%off", "Password with percent"),
        ("with space", "Password with space"),
        ("complex|&@:/#?%", "All special characters"),
    ];

    println!("Testing password encoding:\n");
    println!("{:<30} {:<25} {:<30}", "Description", "Original", "Encoded");
    println!("{}", "-".repeat(85));

    for (password, description) in &test_passwords {
        let encoded = encode(password);
        let changed = password != &encoded.to_string();

        println!(
            "{:<30} {:<25} {:<30} {}",
            description,
            password,
            encoded,
            if changed { "✓ ENCODED" } else { "" }
        );
    }

    println!("\n{}", "=".repeat(85));
    println!("\nMySQL Connection URL Examples:\n");

    // Show how URLs are constructed
    let examples = vec![
        ("root", "simple123", "localhost", 3306, "mydb"),
        ("root", "my|password", "localhost", 3306, "mydb"),
        ("admin", "p@ssw0rd!", "db.example.com", 3306, "production"),
        ("user", "complex|&@:/#?%", "192.168.1.100", 3306, "test-db"),
    ];

    for (username, password, host, port, database) in examples {
        println!("Credentials: {}:{}", username, password);

        // Without encoding (BROKEN)
        let broken_url = format!(
            "mysql://{}:{}@{}:{}/{}",
            username, password, host, port, database
        );

        // With encoding (FIXED)
        let encoded_username = encode(username);
        let encoded_password = encode(password);
        let encoded_database = encode(database);
        let fixed_url = format!(
            "mysql://{}:{}@{}:{}/{}",
            encoded_username, encoded_password, host, port, encoded_database
        );

        println!("  ❌ Broken: {}", broken_url);
        println!("  ✅ Fixed:  {}", fixed_url);
        println!();
    }

    println!("{}", "=".repeat(85));
    println!("\nCharacter Encoding Reference:\n");

    let special_chars = vec![
        ('|', "Pipe"),
        ('&', "Ampersand"),
        ('@', "At sign"),
        (':', "Colon"),
        ('/', "Slash"),
        ('?', "Question mark"),
        ('#', "Hash"),
        ('%', "Percent"),
        (' ', "Space"),
        ('+', "Plus"),
        ('=', "Equals"),
    ];

    println!("{:<20} {:<15} {:<15}", "Character", "Symbol", "Encoded");
    println!("{}", "-".repeat(50));

    for (ch, name) in special_chars {
        let ch_str = ch.to_string();
        let encoded = encode(&ch_str);
        println!("{:<20} {:<15} {:<15}", name, ch, encoded);
    }

    println!("\n{}", "=".repeat(85));
    println!("\n✅ All passwords can now be safely encoded for database connections!");
    println!("✅ No more 'Unknown connection URL parameter' errors!");
    println!("\nFor more information, see: crates/guacr-database/URL_ENCODING_FIX.md\n");
}
