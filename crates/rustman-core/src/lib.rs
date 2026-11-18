//! Rustman engine core: workspace model, environments, variables, secrets and
//! the request model shared by every front-end.
//!
//! This crate has no network, terminal or CLI dependencies.

pub mod assertions;
pub mod captures;
pub mod collection;
pub mod data_rows;
pub mod discovery;
pub mod environment;
pub mod error;
pub mod format;
pub mod history;
pub mod init;
pub mod model;
pub mod paths;
pub mod redact;
pub mod run;
pub mod safety;
pub mod secret;
pub mod secrets;
pub mod settings;
pub mod vars;
pub mod workspace;

pub use environment::Environment;
pub use error::{ConfigError, LoadError, Position};
pub use redact::Redactor;
pub use secret::Secret;
pub use secrets::{SecretBackend, SecretError, SecretStore};
pub use workspace::{RequestRef, Workspace};

/// Version of the Rustman engine.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
