// Database-specific security features
//
// Re-exports security types from guacr-handlers for use by database protocol handlers.
// This module provides backward compatibility for existing code while
// consolidating security logic in the shared guacr-handlers crate.

// Re-export all database security types from the shared security module
pub use guacr_handlers::{
    // Database-specific
    check_sql_query_allowed,
    classify_sql_query,
    is_mysql_export_query,
    is_mysql_import_query,
    is_postgres_copy_in,
    is_postgres_copy_out,
    DatabaseSecuritySettings,
    QueryType,
};

// Convenience aliases for backward compatibility
pub use classify_sql_query as classify_query;

/// Check if a query is allowed - convenience wrapper
pub fn check_query_allowed(query: &str, settings: &DatabaseSecuritySettings) -> Result<(), String> {
    check_sql_query_allowed(query, settings)
}

/// Check if CSV export is allowed
pub fn check_csv_export_allowed(settings: &DatabaseSecuritySettings) -> Result<(), String> {
    if settings.is_csv_export_allowed() {
        Ok(())
    } else {
        Err("CSV export is disabled by your administrator.".to_string())
    }
}

/// Check if CSV import is allowed
pub fn check_csv_import_allowed(settings: &DatabaseSecuritySettings) -> Result<(), String> {
    if settings.is_csv_import_allowed() {
        Ok(())
    } else {
        Err("CSV import is disabled by your administrator.".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_classify_query_readonly() {
        assert_eq!(classify_query("SELECT * FROM users"), QueryType::ReadOnly);
        assert_eq!(classify_query("  select id from t"), QueryType::ReadOnly);
        assert_eq!(classify_query("SHOW DATABASES"), QueryType::ReadOnly);
        assert_eq!(classify_query("DESCRIBE users"), QueryType::ReadOnly);
        assert_eq!(classify_query("DESC users"), QueryType::ReadOnly);
        assert_eq!(classify_query("EXPLAIN SELECT 1"), QueryType::ReadOnly);
        assert_eq!(classify_query("USE database_name"), QueryType::ReadOnly);
    }

    #[test]
    fn test_classify_query_modifying() {
        assert_eq!(
            classify_query("INSERT INTO users VALUES (1)"),
            QueryType::Modifying
        );
        assert_eq!(
            classify_query("UPDATE users SET name='x'"),
            QueryType::Modifying
        );
        assert_eq!(classify_query("DELETE FROM users"), QueryType::Modifying);
        assert_eq!(classify_query("DROP TABLE users"), QueryType::Modifying);
        assert_eq!(
            classify_query("ALTER TABLE users ADD col INT"),
            QueryType::Modifying
        );
        assert_eq!(
            classify_query("CREATE TABLE new_table (id INT)"),
            QueryType::Modifying
        );
        assert_eq!(classify_query("TRUNCATE TABLE users"), QueryType::Modifying);
        assert_eq!(
            classify_query("GRANT SELECT ON users TO 'user'"),
            QueryType::Modifying
        );
    }

    #[test]
    fn test_check_query_allowed_readonly_mode() {
        let mut params = HashMap::new();
        params.insert("read-only".to_string(), "true".to_string());
        let settings = DatabaseSecuritySettings::from_params(&params);

        assert!(check_query_allowed("SELECT * FROM users", &settings).is_ok());
        assert!(check_query_allowed("INSERT INTO users VALUES (1)", &settings).is_err());
        assert!(check_query_allowed("DROP TABLE users", &settings).is_err());
    }

    #[test]
    fn test_check_query_allowed_normal_mode() {
        let settings = DatabaseSecuritySettings::default();

        assert!(check_query_allowed("SELECT * FROM users", &settings).is_ok());
        assert!(check_query_allowed("INSERT INTO users VALUES (1)", &settings).is_ok());
        assert!(check_query_allowed("DROP TABLE users", &settings).is_ok());
    }

    #[test]
    fn test_from_params() {
        let mut params = HashMap::new();
        params.insert("read-only".to_string(), "true".to_string());
        params.insert("disable-copy".to_string(), "1".to_string());
        params.insert("disable-csv-export".to_string(), "true".to_string());

        let settings = DatabaseSecuritySettings::from_params(&params);

        assert!(settings.base.read_only);
        assert!(settings.base.disable_copy);
        assert!(!settings.base.disable_paste);
        assert!(settings.disable_csv_export);
        assert!(!settings.disable_csv_import);
    }

    #[test]
    fn test_csv_export_import_allowed() {
        let mut params = HashMap::new();
        params.insert("disable-csv-export".to_string(), "true".to_string());
        let settings = DatabaseSecuritySettings::from_params(&params);

        assert!(check_csv_export_allowed(&settings).is_err());
        assert!(check_csv_import_allowed(&settings).is_ok());

        // read-only mode blocks import too
        let mut params2 = HashMap::new();
        params2.insert("read-only".to_string(), "true".to_string());
        let settings2 = DatabaseSecuritySettings::from_params(&params2);
        assert!(check_csv_import_allowed(&settings2).is_err());
    }

    #[test]
    fn test_mysql_export_detection() {
        assert!(is_mysql_export_query(
            "SELECT * FROM users INTO OUTFILE '/tmp/users.csv'"
        ));
        assert!(is_mysql_export_query(
            "SELECT * FROM users INTO LOCAL OUTFILE 'users.csv'"
        ));
        assert!(!is_mysql_export_query("SELECT * FROM users"));
    }

    #[test]
    fn test_mysql_import_detection() {
        assert!(is_mysql_import_query(
            "LOAD DATA INFILE '/tmp/users.csv' INTO TABLE users"
        ));
        assert!(is_mysql_import_query(
            "LOAD LOCAL DATA INFILE 'users.csv' INTO TABLE users"
        ));
        assert!(!is_mysql_import_query("SELECT * FROM users"));
    }

    #[test]
    fn test_postgres_copy_detection() {
        assert!(is_postgres_copy_out("COPY users TO STDOUT"));
        assert!(is_postgres_copy_in("COPY users FROM STDIN"));
        assert!(!is_postgres_copy_out("COPY users FROM STDIN"));
        assert!(!is_postgres_copy_in("COPY users TO STDOUT"));
    }
}
