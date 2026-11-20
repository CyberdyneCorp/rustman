//! Plugin host for Rustman (add-plugin-system): sandboxed WebAssembly
//! component plugins hosted with wasmtime.
//!
//! - [`declare`], [`manifest`], [`grant`] and [`limits`] read what the
//!   workspace declares and what a plugin asks for;
//! - [`pin`], [`lock`], [`source`] and [`fetch`] make sure the bytes that
//!   run are the pinned ones, and [`files`] reads what a workspace commits
//!   without trusting it;
//! - [`workspace`] loads a workspace's plugin set without running it;
//! - [`sandbox`] and [`plugin`] run a verified plugin with exactly its
//!   grants, under per-call CPU and memory limits;
//! - [`trust`] decides whether a workspace's plugin set may load, and
//!   [`runtime`] serves the extension points of a trusted set;
//! - [`manage`] (with [`edit`]) adds, removes, lists and verifies a
//!   workspace's plugins, [`template`] scaffolds new ones and [`harness`]
//!   tests them against fake services.

mod bindings;
pub mod declare;
pub mod edit;
pub mod error;
pub mod extension;
pub mod fetch;
pub mod files;
#[cfg(feature = "test-fixtures")]
pub mod fixtures;
pub mod grant;
pub mod harness;
pub mod limits;
pub mod lock;
pub mod manage;
pub mod manifest;
pub mod pin;
pub mod plugin;
mod points;
pub mod runtime;
pub mod sandbox;
pub mod session;
pub mod source;
pub mod template;
pub mod trust;
pub mod workspace;

pub use error::LoadError;
pub use extension::ExtensionPoint;
pub use manifest::{Manifest, SUPPORTED_API};
pub use plugin::{CallError, Plugin};
pub use runtime::PluginRuntime;
pub use workspace::{LoadOptions, PluginSet, VerifiedPlugin, load, verify_pins};
