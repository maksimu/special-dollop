//! Authentication credentials returned by AuthProvider.
//!
//! This module provides the [`AuthCredentials`] struct which contains
//! the credentials and configuration returned by an [`AuthProvider`](crate::auth::AuthProvider).

use std::collections::HashMap;

use super::AuthMethod;

/// Authentication credentials returned by [`AuthProvider::get_auth`](crate::auth::AuthProvider::get_auth).
///
/// Contains the authentication method with credentials, plus optional
/// overrides for database name and connection parameters.
///
/// # Example
///
/// ```
/// use keeperdb_proxy::auth::{AuthCredentials, AuthMethod};
///
/// let creds = AuthCredentials::new(AuthMethod::mysql_native("user", "pass"))
///     .with_database("mydb")
///     .with_param("charset", "utf8mb4");
///
/// assert_eq!(creds.database, Some("mydb".to_string()));
/// ```
#[derive(Clone)]
pub struct AuthCredentials {
    /// The authentication method with credentials.
    pub method: AuthMethod,

    /// Database name override.
    ///
    /// If set, this overrides any database name specified by the client.
    pub database: Option<String>,

    /// Additional connection parameters.
    ///
    /// Database-specific parameters like charset, timezone, etc.
    pub connection_params: HashMap<String, String>,
}

impl AuthCredentials {
    /// Create new credentials with the given authentication method.
    ///
    /// # Arguments
    ///
    /// * `method` - The authentication method to use
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::{AuthCredentials, AuthMethod};
    ///
    /// let creds = AuthCredentials::new(AuthMethod::mysql_native("root", "secret"));
    /// assert!(creds.database.is_none());
    /// ```
    pub fn new(method: AuthMethod) -> Self {
        Self {
            method,
            database: None,
            connection_params: HashMap::new(),
        }
    }

    /// Set the database name override (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `db` - The database name to use
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::{AuthCredentials, AuthMethod};
    ///
    /// let creds = AuthCredentials::new(AuthMethod::mysql_native("user", "pass"))
    ///     .with_database("production_db");
    ///
    /// assert_eq!(creds.database, Some("production_db".to_string()));
    /// ```
    pub fn with_database(mut self, db: impl Into<String>) -> Self {
        self.database = Some(db.into());
        self
    }

    /// Add a connection parameter (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `key` - Parameter name
    /// * `value` - Parameter value
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::{AuthCredentials, AuthMethod};
    ///
    /// let creds = AuthCredentials::new(AuthMethod::mysql_native("user", "pass"))
    ///     .with_param("charset", "utf8mb4")
    ///     .with_param("timezone", "+00:00");
    ///
    /// assert_eq!(creds.connection_params.get("charset"), Some(&"utf8mb4".to_string()));
    /// ```
    pub fn with_param(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.connection_params.insert(key.into(), value.into());
        self
    }
}

// Custom Debug that uses AuthMethod's redacting Debug
impl std::fmt::Debug for AuthCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthCredentials")
            .field("method", &self.method)
            .field("database", &self.database)
            .field("connection_params", &self.connection_params)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_with_method() {
        let creds = AuthCredentials::new(AuthMethod::mysql_native("user", "pass"));
        assert_eq!(creds.method.username(), "user");
        assert!(creds.database.is_none());
        assert!(creds.connection_params.is_empty());
    }

    #[test]
    fn test_with_database() {
        let creds =
            AuthCredentials::new(AuthMethod::mysql_native("user", "pass")).with_database("testdb");
        assert_eq!(creds.database, Some("testdb".to_string()));
    }

    #[test]
    fn test_with_param() {
        let creds = AuthCredentials::new(AuthMethod::mysql_native("user", "pass"))
            .with_param("charset", "utf8")
            .with_param("timeout", "30");

        assert_eq!(
            creds.connection_params.get("charset"),
            Some(&"utf8".to_string())
        );
        assert_eq!(
            creds.connection_params.get("timeout"),
            Some(&"30".to_string())
        );
    }

    #[test]
    fn test_debug_redacts_credentials() {
        let creds = AuthCredentials::new(AuthMethod::mysql_native("user", "secret123"))
            .with_database("mydb");
        let debug_output = format!("{:?}", creds);

        assert!(!debug_output.contains("secret123"));
        assert!(debug_output.contains("[REDACTED]"));
        assert!(debug_output.contains("user"));
        assert!(debug_output.contains("mydb"));
    }

    #[test]
    fn test_clone() {
        let creds1 = AuthCredentials::new(AuthMethod::mysql_native("user", "pass"))
            .with_database("db")
            .with_param("key", "value");
        let creds2 = creds1.clone();

        assert_eq!(creds1.method.username(), creds2.method.username());
        assert_eq!(creds1.database, creds2.database);
        assert_eq!(creds1.connection_params, creds2.connection_params);
    }
}
