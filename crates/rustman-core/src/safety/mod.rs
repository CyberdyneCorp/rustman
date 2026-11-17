//! The safety policy (design D5): the rules that decide whether an automated
//! request may be sent. Method gating, the origin allowlist and the production
//! guard live here, with the `[safety]` settings, and so do the rate limit,
//! concurrency, `429` backoff and budgets ([`Governor`]). The governed sender
//! that enforces all of it over HTTP is `rustman_http::GovernedSender`; this
//! crate stays free of network code.
//!
//! See `docs/safety.md`.

mod config;
mod limits;
mod method;
mod origin;
mod policy;
mod production;

pub use config::{DEFAULT_CONCURRENCY, DEFAULT_RATE, SafetySettings};
pub use limits::{
    Admission, Backoff, BudgetExhausted, DEFAULT_MAX_DURATION, DEFAULT_MAX_PROBES, Governor,
    Limits, Paced, TOO_MANY_REQUESTS, parse_retry_after,
};
pub use method::{MethodClass, Permission};
pub use origin::Origin;
pub use policy::{Blocked, PolicyError, PolicyOptions, SafetyPolicy};
pub use production::{
    EnvironmentInfo, HostPattern, PRODUCTION_ENVIRONMENT_NAMES, ProductionReason, detect,
};
