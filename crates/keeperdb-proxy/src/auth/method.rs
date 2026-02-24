//! Authentication method definitions.
//!
//! This module provides the [`AuthMethod`] enum which represents different
//! authentication methods supported by the proxy. All credential fields
//! use [`Zeroizing<String>`] to ensure secure memory handling.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use zeroize::{ZeroizeOnDrop, Zeroizing};

/// Authentication method with credentials.
///
/// All password and token fields use [`Zeroizing<String>`] to ensure
/// credentials are securely erased from memory when dropped.
///
/// # Security
///
/// - Passwords are automatically zeroed when the enum is dropped
/// - The [`Debug`] implementation redacts all sensitive fields
/// - Cloning creates independent copies that are both zeroized on drop
///
/// # Example
///
/// ```
/// use keeperdb_proxy::auth::AuthMethod;
///
/// let method = AuthMethod::mysql_native("db_user", "secret_password");
/// assert_eq!(method.username(), "db_user");
///
/// // Password is redacted in debug output
/// let debug = format!("{:?}", method);
/// assert!(!debug.contains("secret_password"));
/// assert!(debug.contains("[REDACTED]"));
/// ```
#[derive(Clone, ZeroizeOnDrop)]
pub enum AuthMethod {
    /// MySQL mysql_native_password authentication (SHA1-based).
    ///
    /// This is the traditional MySQL authentication method.
    MysqlNativePassword {
        /// Database username
        #[zeroize(skip)]
        username: String,
        /// Database password (zeroized on drop)
        password: Zeroizing<String>,
    },

    /// MySQL caching_sha2_password authentication (SHA256-based).
    ///
    /// This is the default authentication method in MySQL 8.0+.
    MysqlCachingSha2 {
        /// Database username
        #[zeroize(skip)]
        username: String,
        /// Database password (zeroized on drop)
        password: Zeroizing<String>,
    },

    /// PostgreSQL MD5 authentication.
    ///
    /// Traditional PostgreSQL password authentication.
    PostgresMd5 {
        /// Database username
        #[zeroize(skip)]
        username: String,
        /// Database password (zeroized on drop)
        password: Zeroizing<String>,
    },

    /// PostgreSQL SCRAM-SHA-256 authentication.
    ///
    /// Modern PostgreSQL authentication (PostgreSQL 10+).
    PostgresScramSha256 {
        /// Database username
        #[zeroize(skip)]
        username: String,
        /// Database password (zeroized on drop)
        password: Zeroizing<String>,
    },

    /// Client certificate authentication.
    ///
    /// Uses TLS client certificates for authentication.
    Certificate {
        /// Path to client certificate (PEM format)
        #[zeroize(skip)]
        cert_path: PathBuf,
        /// Path to client private key (PEM format)
        #[zeroize(skip)]
        key_path: PathBuf,
        /// Password for encrypted private key (optional)
        key_password: Option<Zeroizing<String>>,
    },

    /// AWS IAM authentication for RDS/Aurora.
    ///
    /// Uses short-lived IAM tokens for database access.
    AwsIam {
        /// Database username
        #[zeroize(skip)]
        username: String,
        /// IAM authentication token (zeroized on drop)
        token: Zeroizing<String>,
        /// AWS region
        #[zeroize(skip)]
        region: String,
        /// Token expiration time
        #[zeroize(skip)]
        expires_at: DateTime<Utc>,
    },

    /// SQL Server SQL Authentication.
    ///
    /// Traditional SQL Server username/password authentication.
    SqlServerAuth {
        /// Database username
        #[zeroize(skip)]
        username: String,
        /// Database password (zeroized on drop)
        password: Zeroizing<String>,
    },
}

impl AuthMethod {
    /// Get the username for this authentication method.
    ///
    /// Returns an empty string for certificate authentication,
    /// as the username is derived from the certificate's CN.
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::AuthMethod;
    ///
    /// let method = AuthMethod::mysql_native("admin", "password");
    /// assert_eq!(method.username(), "admin");
    /// ```
    pub fn username(&self) -> &str {
        match self {
            Self::MysqlNativePassword { username, .. } => username,
            Self::MysqlCachingSha2 { username, .. } => username,
            Self::PostgresMd5 { username, .. } => username,
            Self::PostgresScramSha256 { username, .. } => username,
            Self::Certificate { .. } => "", // CN from certificate
            Self::AwsIam { username, .. } => username,
            Self::SqlServerAuth { username, .. } => username,
        }
    }

    /// Check if this is a password-based authentication method.
    ///
    /// Returns `true` for methods that use username/password credentials.
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::AuthMethod;
    ///
    /// let native = AuthMethod::mysql_native("user", "pass");
    /// assert!(native.is_password_based());
    /// ```
    pub fn is_password_based(&self) -> bool {
        matches!(
            self,
            Self::MysqlNativePassword { .. }
                | Self::MysqlCachingSha2 { .. }
                | Self::PostgresMd5 { .. }
                | Self::PostgresScramSha256 { .. }
                | Self::SqlServerAuth { .. }
        )
    }

    /// Create a MySQL native password authentication method.
    ///
    /// # Arguments
    ///
    /// * `username` - Database username
    /// * `password` - Database password
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::AuthMethod;
    ///
    /// let method = AuthMethod::mysql_native("root", "secret");
    /// assert_eq!(method.username(), "root");
    /// ```
    pub fn mysql_native(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self::MysqlNativePassword {
            username: username.into(),
            password: Zeroizing::new(password.into()),
        }
    }

    /// Create a MySQL caching_sha2 authentication method.
    ///
    /// # Arguments
    ///
    /// * `username` - Database username
    /// * `password` - Database password
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::AuthMethod;
    ///
    /// let method = AuthMethod::mysql_caching_sha2("admin", "password");
    /// assert_eq!(method.username(), "admin");
    /// ```
    pub fn mysql_caching_sha2(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self::MysqlCachingSha2 {
            username: username.into(),
            password: Zeroizing::new(password.into()),
        }
    }

    /// Create a PostgreSQL MD5 authentication method.
    ///
    /// # Arguments
    ///
    /// * `username` - Database username
    /// * `password` - Database password
    pub fn postgres_md5(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self::PostgresMd5 {
            username: username.into(),
            password: Zeroizing::new(password.into()),
        }
    }

    /// Create a PostgreSQL SCRAM-SHA-256 authentication method.
    ///
    /// # Arguments
    ///
    /// * `username` - Database username
    /// * `password` - Database password
    pub fn postgres_scram_sha256(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self::PostgresScramSha256 {
            username: username.into(),
            password: Zeroizing::new(password.into()),
        }
    }

    /// Create a SQL Server SQL Authentication method.
    ///
    /// # Arguments
    ///
    /// * `username` - Database username
    /// * `password` - Database password
    pub fn sqlserver_auth(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self::SqlServerAuth {
            username: username.into(),
            password: Zeroizing::new(password.into()),
        }
    }

    /// Get the password for password-based methods.
    ///
    /// Returns `None` for non-password methods (Certificate, AwsIam).
    ///
    /// # Security
    ///
    /// The returned reference should be used immediately and not stored.
    /// The password will be zeroized when the AuthMethod is dropped.
    pub fn password(&self) -> Option<&str> {
        match self {
            Self::MysqlNativePassword { password, .. } => Some(password.as_str()),
            Self::MysqlCachingSha2 { password, .. } => Some(password.as_str()),
            Self::PostgresMd5 { password, .. } => Some(password.as_str()),
            Self::PostgresScramSha256 { password, .. } => Some(password.as_str()),
            Self::SqlServerAuth { password, .. } => Some(password.as_str()),
            Self::Certificate { .. } => None,
            Self::AwsIam { .. } => None,
        }
    }
}

// Custom Debug implementation that redacts sensitive fields
impl std::fmt::Debug for AuthMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MysqlNativePassword { username, .. } => f
                .debug_struct("MysqlNativePassword")
                .field("username", username)
                .field("password", &"[REDACTED]")
                .finish(),
            Self::MysqlCachingSha2 { username, .. } => f
                .debug_struct("MysqlCachingSha2")
                .field("username", username)
                .field("password", &"[REDACTED]")
                .finish(),
            Self::PostgresMd5 { username, .. } => f
                .debug_struct("PostgresMd5")
                .field("username", username)
                .field("password", &"[REDACTED]")
                .finish(),
            Self::PostgresScramSha256 { username, .. } => f
                .debug_struct("PostgresScramSha256")
                .field("username", username)
                .field("password", &"[REDACTED]")
                .finish(),
            Self::Certificate {
                cert_path,
                key_path,
                key_password,
            } => f
                .debug_struct("Certificate")
                .field("cert_path", cert_path)
                .field("key_path", key_path)
                .field("key_password", &key_password.as_ref().map(|_| "[REDACTED]"))
                .finish(),
            Self::AwsIam {
                username,
                region,
                expires_at,
                ..
            } => f
                .debug_struct("AwsIam")
                .field("username", username)
                .field("token", &"[REDACTED]")
                .field("region", region)
                .field("expires_at", expires_at)
                .finish(),
            Self::SqlServerAuth { username, .. } => f
                .debug_struct("SqlServerAuth")
                .field("username", username)
                .field("password", &"[REDACTED]")
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mysql_native_creation() {
        let method = AuthMethod::mysql_native("testuser", "testpass");
        assert_eq!(method.username(), "testuser");
        assert_eq!(method.password(), Some("testpass"));
    }

    #[test]
    fn test_mysql_caching_sha2_creation() {
        let method = AuthMethod::mysql_caching_sha2("user", "pass");
        assert_eq!(method.username(), "user");
        assert_eq!(method.password(), Some("pass"));
    }

    #[test]
    fn test_postgres_md5_creation() {
        let method = AuthMethod::postgres_md5("pguser", "pgpass");
        assert_eq!(method.username(), "pguser");
        assert_eq!(method.password(), Some("pgpass"));
    }

    #[test]
    fn test_postgres_scram_creation() {
        let method = AuthMethod::postgres_scram_sha256("pguser", "pgpass");
        assert_eq!(method.username(), "pguser");
        assert_eq!(method.password(), Some("pgpass"));
    }

    #[test]
    fn test_certificate_creation() {
        let method = AuthMethod::Certificate {
            cert_path: PathBuf::from("/path/to/cert.pem"),
            key_path: PathBuf::from("/path/to/key.pem"),
            key_password: Some(Zeroizing::new("keypass".to_string())),
        };
        assert_eq!(method.username(), "");
        assert_eq!(method.password(), None);
        assert!(!method.is_password_based());
    }

    #[test]
    fn test_aws_iam_creation() {
        let method = AuthMethod::AwsIam {
            username: "iam_user".to_string(),
            token: Zeroizing::new("token123".to_string()),
            region: "us-east-1".to_string(),
            expires_at: Utc::now(),
        };
        assert_eq!(method.username(), "iam_user");
        assert_eq!(method.password(), None);
        assert!(!method.is_password_based());
    }

    #[test]
    fn test_is_password_based() {
        assert!(AuthMethod::mysql_native("u", "p").is_password_based());
        assert!(AuthMethod::mysql_caching_sha2("u", "p").is_password_based());
        assert!(AuthMethod::postgres_md5("u", "p").is_password_based());
        assert!(AuthMethod::postgres_scram_sha256("u", "p").is_password_based());
    }

    #[test]
    fn test_debug_redacts_password() {
        let method = AuthMethod::mysql_native("testuser", "supersecret123");
        let debug_output = format!("{:?}", method);

        assert!(
            !debug_output.contains("supersecret123"),
            "Password should not appear in debug output"
        );
        assert!(
            debug_output.contains("[REDACTED]"),
            "Should contain [REDACTED]"
        );
        assert!(debug_output.contains("testuser"), "Username should appear");
    }

    #[test]
    fn test_debug_redacts_token() {
        let method = AuthMethod::AwsIam {
            username: "iam_user".to_string(),
            token: Zeroizing::new("secret_token_xyz".to_string()),
            region: "us-west-2".to_string(),
            expires_at: Utc::now(),
        };
        let debug_output = format!("{:?}", method);

        assert!(
            !debug_output.contains("secret_token_xyz"),
            "Token should not appear in debug output"
        );
        assert!(debug_output.contains("[REDACTED]"));
        assert!(debug_output.contains("iam_user"));
        assert!(debug_output.contains("us-west-2"));
    }

    #[test]
    fn test_debug_redacts_key_password() {
        let method = AuthMethod::Certificate {
            cert_path: PathBuf::from("/cert.pem"),
            key_path: PathBuf::from("/key.pem"),
            key_password: Some(Zeroizing::new("key_secret".to_string())),
        };
        let debug_output = format!("{:?}", method);

        assert!(!debug_output.contains("key_secret"));
        assert!(debug_output.contains("[REDACTED]"));
    }

    #[test]
    fn test_clone_independence() {
        let method1 = AuthMethod::mysql_native("user", "password");
        let method2 = method1.clone();

        // Both should have the same values
        assert_eq!(method1.username(), method2.username());
        assert_eq!(method1.password(), method2.password());

        // Dropping one shouldn't affect the other
        drop(method1);
        assert_eq!(method2.username(), "user");
        assert_eq!(method2.password(), Some("password"));
    }
}
