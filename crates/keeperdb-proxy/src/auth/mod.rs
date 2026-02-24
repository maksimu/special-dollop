//! Authentication provider module.
//!
//! This module provides a pluggable authentication architecture for the proxy.
//! The core abstraction is the [`AuthProvider`] trait, which allows different
//! credential sources (static config, Gateway, etc.) to be used interchangeably.
//!
//! # Overview
//!
//! The authentication system consists of:
//!
//! - [`AuthProvider`] - Trait for credential retrieval
//! - [`AuthMethod`] - Enum of supported authentication methods
//! - [`AuthCredentials`] - Credentials returned by providers
//! - [`ConnectionContext`] - Metadata about incoming connections
//! - [`SessionConfig`] - Session limits and configuration
//! - [`TokenValidation`] - Result of token validation
//! - [`StaticAuthProvider`] - Provider for static YAML configuration
//!
//! # Security
//!
//! All credential fields use [`zeroize::Zeroizing`] to ensure passwords
//! are securely erased from memory when dropped. Custom [`Debug`]
//! implementations redact sensitive fields.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use keeperdb_proxy::auth::{
//!     AuthMethod, AuthProvider, ConnectionContext,
//!     DatabaseType, StaticAuthProvider,
//! };
//!
//! // Create a static provider
//! let provider: Arc<dyn AuthProvider> = Arc::new(
//!     StaticAuthProvider::new(AuthMethod::mysql_native("user", "password"))
//! );
//!
//! // Create connection context
//! let context = ConnectionContext::new(
//!     "127.0.0.1:12345".parse().unwrap(),
//!     "db.example.com",
//!     3306,
//!     DatabaseType::MySQL,
//! );
//! ```

mod context;
mod credentials;
mod method;
mod provider;
mod session;
mod static_provider;
mod token;

pub use context::{ConnectionContext, DatabaseType};
pub use credentials::AuthCredentials;
pub use method::AuthMethod;
pub use provider::AuthProvider;
pub use session::SessionConfig;
pub use static_provider::StaticAuthProvider;
pub use token::TokenValidation;
