//! Configuration module for keeperdb-proxy
//!
//! Supports both single-target and multi-target configurations:
//!
//! ## Single-target mode (backwards compatible)
//! ```yaml
//! target:
//!   host: "localhost"
//!   port: 3306
//! credentials:
//!   username: "root"
//!   password: "secret"
//! ```
//!
//! ## Multi-target mode
//! ```yaml
//! targets:
//!   mysql:
//!     host: "mysql.example.com"
//!     port: 3306
//!     credentials:
//!       username: "root"
//!       password: "secret"
//!   postgresql:
//!     host: "postgres.example.com"
//!     port: 5432
//!     credentials:
//!       username: "postgres"
//!       password: "secret"
//! ```

mod loader;
mod types;

pub use loader::{apply_env_overrides, load_config, load_config_from_str};
pub use types::*;
