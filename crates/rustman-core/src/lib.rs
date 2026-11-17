//! Rustman engine core: workspace model, environments, variables, secrets and
//! the request model shared by every front-end.
//!
//! This crate has no network, terminal or CLI dependencies.

/// Version of the Rustman engine.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
