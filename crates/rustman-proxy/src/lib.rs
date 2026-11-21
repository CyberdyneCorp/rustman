//! Traffic recorder (add-traffic-recorder): a local recording proxy.
//!
//! - [`Proxy`]: an HTTP forward proxy on loopback by default. Absolute-form
//!   requests are forwarded upstream through `rustman-http`'s connector and
//!   recorded; `CONNECT` tunnels are relayed opaquely unless their host is
//!   in the interception scope, in which case TLS is terminated with a leaf
//!   certificate from the local CA.
//! - [`ca`]: the per-workspace local CA, its private key in the OS keychain
//!   only, installed into trust stores only on explicit commands.
//! - [`CaptureStore`]: the bounded capture list front-ends read.
//!
//! hyper does all HTTP parsing and framing (design D1); bodies stream
//! through, and only their first `max_body` bytes are kept.

pub mod body;
pub mod ca;
pub mod capture;
pub mod config;
pub mod filter;
pub mod har;
pub mod jsonl;
pub mod save;
pub mod scope;
pub mod store;

mod forward;
mod guard;
mod idle;
mod intercept;
mod named;
mod recorder;
mod server;
mod tunnel;

pub use capture::{BodyCapture, Capture, Exchange, Recorded, TLS_REJECTED_HINT, Tunnel};
pub use config::{BindError, ProxyConfig, RecordSettings};
pub use filter::{CaptureFilter, FilterRules};
pub use server::{Proxy, ProxyError, banner};
pub use store::CaptureStore;
