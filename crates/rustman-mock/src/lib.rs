//! OpenAPI mock server (add-openapi-mock-server): serves the operations of
//! an OpenAPI document over HTTP/1.1 and h2c, with routes compiled from its
//! paths, responses chosen from its examples (or synthesized from its
//! schemas with a seed), content negotiation and `Prefer` support, the
//! request limits of the webhook inspector, request validation, optional
//! security enforcement, hand-written overrides with templates, latency
//! and faults, an optional stateful store, CORS, a request log, hot
//! reload, and forwarding (and recording) of unmatched requests.
//!
//! The document is read by `rustman-explore` (the `openapi-import` parser,
//! `$ref` policy and synthesizer); the bind rules and limits come from
//! `rustman-webhook`. Front-ends (CLI, TUI) load a [`MockState`], bind a
//! [`MockServer`] and stop it through its [`MockHandle`].

pub mod config;
pub mod cors;
pub mod credentials;
pub mod faults;
pub mod log;
pub mod negotiate;
pub mod overrides;
pub mod prefer;
pub mod problem;
pub mod proxy;
pub mod record;
pub mod request;
pub mod respond;
pub mod router;
pub mod security;
pub mod server;
pub mod settings;
pub mod state;
pub mod stateful;
pub mod template;
pub mod validate;
pub mod watch;

mod service;

pub use config::MockConfig;
pub use log::{LogEntry, MockEvent};
pub use overrides::Overrides;
pub use proxy::Upstream;
pub use record::Recorder;
pub use respond::Source;
pub use server::{MockHandle, MockServer, Reload, Reloader, ServeError};
pub use settings::MockSettings;
pub use state::{MockState, load};
