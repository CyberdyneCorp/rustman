//! Webhook inspector (add-webhook-inspector): a local listener that captures
//! inbound HTTP callbacks, answers them with a default acknowledgement or a
//! configured mock, and reports every capture, rejection and mock match on
//! an event log; checks request signatures (GitHub, Stripe, generic HMAC);
//! and replays or saves captures with header hygiene.
//!
//! All HTTP parsing and framing is done by `hyper` (design D1); every limit
//! is enforced before the memory it guards is committed. Front-ends (CLI,
//! TUI) start a [`Listener`] and read the shared [`CaptureStore`].

pub mod body;
pub mod capture;
pub mod config;
pub mod curl;
pub mod event;
pub mod jsonl;
pub mod listener;
pub mod mocks;
pub mod replay;
pub mod save;
pub mod settings;
pub mod signature;
pub mod store;
pub mod verifiers;
pub mod watch;

mod conn;
mod head;
mod service;

pub use capture::{Capture, HeaderEntry};
pub use config::{BufferLimits, Limits, ListenerConfig};
pub use event::{Event, Rejection};
pub use listener::{ListenError, Listener, ListenerHandle};
pub use mocks::{Mock, Mocks};
pub use replay::{replay, sanitize_for_replay};
pub use service::DEFAULT_ACK;
pub use settings::WebhookSettings;
pub use signature::Signature;
pub use store::CaptureStore;
pub use verifiers::{Verifier, VerifierSpec, Verifiers};
