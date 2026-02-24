//! Token validation results.
//!
//! This module provides the [`TokenValidation`] struct which contains
//! the result of validating a connection token.

use chrono::{DateTime, Utc};

/// Result of token validation.
///
/// Returned by [`AuthProvider::validate_token`](crate::auth::AuthProvider::validate_token) to indicate whether
/// a connection token is valid and any associated metadata.
///
/// # Example
///
/// ```
/// use keeperdb_proxy::auth::TokenValidation;
///
/// // Create valid result
/// let valid = TokenValidation::valid();
/// assert!(valid.valid);
///
/// // Create invalid result
/// let invalid = TokenValidation::invalid("Token expired");
/// assert!(!invalid.valid);
/// assert_eq!(invalid.error, Some("Token expired".to_string()));
/// ```
#[derive(Debug, Clone)]
pub struct TokenValidation {
    /// Whether the token is valid.
    pub valid: bool,

    /// Error message if the token is invalid.
    pub error: Option<String>,

    /// Token expiration time (if valid and known).
    pub expires_at: Option<DateTime<Utc>>,

    /// Associated PAM record UID (for Gateway integration).
    pub record_uid: Option<String>,
}

impl TokenValidation {
    /// Create a valid token result.
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::TokenValidation;
    ///
    /// let result = TokenValidation::valid();
    /// assert!(result.valid);
    /// assert!(result.error.is_none());
    /// ```
    pub fn valid() -> Self {
        Self {
            valid: true,
            error: None,
            expires_at: None,
            record_uid: None,
        }
    }

    /// Create an invalid token result with an error message.
    ///
    /// # Arguments
    ///
    /// * `error` - Description of why the token is invalid
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::TokenValidation;
    ///
    /// let result = TokenValidation::invalid("Token not found");
    /// assert!(!result.valid);
    /// assert_eq!(result.error, Some("Token not found".to_string()));
    /// ```
    pub fn invalid(error: impl Into<String>) -> Self {
        Self {
            valid: false,
            error: Some(error.into()),
            expires_at: None,
            record_uid: None,
        }
    }

    /// Set the expiration time (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `time` - When the token expires
    ///
    /// # Example
    ///
    /// ```
    /// use chrono::Utc;
    /// use keeperdb_proxy::auth::TokenValidation;
    ///
    /// let result = TokenValidation::valid()
    ///     .with_expires_at(Utc::now());
    ///
    /// assert!(result.expires_at.is_some());
    /// ```
    pub fn with_expires_at(mut self, time: DateTime<Utc>) -> Self {
        self.expires_at = Some(time);
        self
    }

    /// Set the record UID (builder pattern).
    ///
    /// # Arguments
    ///
    /// * `uid` - The PAM record UID associated with this token
    ///
    /// # Example
    ///
    /// ```
    /// use keeperdb_proxy::auth::TokenValidation;
    ///
    /// let result = TokenValidation::valid()
    ///     .with_record_uid("abc123");
    ///
    /// assert_eq!(result.record_uid, Some("abc123".to_string()));
    /// ```
    pub fn with_record_uid(mut self, uid: impl Into<String>) -> Self {
        self.record_uid = Some(uid.into());
        self
    }

    /// Check if the token has expired.
    ///
    /// Returns `true` if an expiration time is set and it's in the past.
    /// Returns `false` if no expiration is set or it's in the future.
    ///
    /// # Example
    ///
    /// ```
    /// use chrono::{Duration, Utc};
    /// use keeperdb_proxy::auth::TokenValidation;
    ///
    /// let future = TokenValidation::valid()
    ///     .with_expires_at(Utc::now() + Duration::hours(1));
    /// assert!(!future.is_expired());
    ///
    /// let past = TokenValidation::valid()
    ///     .with_expires_at(Utc::now() - Duration::hours(1));
    /// assert!(past.is_expired());
    /// ```
    pub fn is_expired(&self) -> bool {
        self.expires_at.is_some_and(|exp| exp < Utc::now())
    }
}

impl Default for TokenValidation {
    fn default() -> Self {
        Self::valid()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_valid_creation() {
        let result = TokenValidation::valid();
        assert!(result.valid);
        assert!(result.error.is_none());
        assert!(result.expires_at.is_none());
        assert!(result.record_uid.is_none());
    }

    #[test]
    fn test_invalid_with_error() {
        let result = TokenValidation::invalid("Token expired");
        assert!(!result.valid);
        assert_eq!(result.error, Some("Token expired".to_string()));
    }

    #[test]
    fn test_with_expires_at() {
        let now = Utc::now();
        let result = TokenValidation::valid().with_expires_at(now);
        assert_eq!(result.expires_at, Some(now));
    }

    #[test]
    fn test_with_record_uid() {
        let result = TokenValidation::valid().with_record_uid("record123");
        assert_eq!(result.record_uid, Some("record123".to_string()));
    }

    #[test]
    fn test_is_expired_future() {
        let result = TokenValidation::valid().with_expires_at(Utc::now() + Duration::hours(1));
        assert!(!result.is_expired());
    }

    #[test]
    fn test_is_expired_past() {
        let result = TokenValidation::valid().with_expires_at(Utc::now() - Duration::hours(1));
        assert!(result.is_expired());
    }

    #[test]
    fn test_is_expired_none() {
        let result = TokenValidation::valid();
        assert!(!result.is_expired()); // No expiration = not expired
    }

    #[test]
    fn test_builder_chain() {
        let now = Utc::now();
        let result = TokenValidation::valid()
            .with_expires_at(now)
            .with_record_uid("uid123");

        assert!(result.valid);
        assert_eq!(result.expires_at, Some(now));
        assert_eq!(result.record_uid, Some("uid123".to_string()));
    }

    #[test]
    fn test_default() {
        let result = TokenValidation::default();
        assert!(result.valid);
    }

    #[test]
    fn test_clone() {
        let result1 = TokenValidation::valid()
            .with_expires_at(Utc::now())
            .with_record_uid("test");
        let result2 = result1.clone();

        assert_eq!(result1.valid, result2.valid);
        assert_eq!(result1.expires_at, result2.expires_at);
        assert_eq!(result1.record_uid, result2.record_uid);
    }
}
