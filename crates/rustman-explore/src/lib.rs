//! OpenAPI import and exploration for Rustman.
//!
//! [`openapi`] reads an OpenAPI 3.0 or 3.1 document (a file or a URL),
//! resolves its references, validates it and lowers it into the [`ApiModel`]
//! IR that every later stage consumes. [`collection`] turns that model into
//! workspace request files, environments and auth config, with example
//! bodies from [`synth`]. [`store`] writes a first import and its snapshot;
//! [`sync`] three-way merges a new document into an imported collection.
//!
//! [`explore`] plans and runs probes against a target under the safety
//! policy and turns what it observes into findings.
//!
//! [`mutate`] mutates saved JSON bodies for stress fuzzing, schema-less or
//! from a document's schemas.
//!
//! This crate holds no UI code; the CLI and TUI are thin adapters over it.

pub mod collection;
pub mod explore;
pub mod mutate;
pub mod openapi;
pub mod store;
pub mod sync;
pub mod synth;

pub use openapi::model::ApiModel;
pub use openapi::{ImportError, ImportOptions, Imported, import};
