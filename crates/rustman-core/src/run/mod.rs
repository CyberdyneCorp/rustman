//! Running collections (cli-runner spec "Run collections", design D10): which
//! requests a run executes and in what units ([`plan`]), and what each
//! request produced ([`record`]), already redacted for every output sink.
//!
//! Executing a plan needs the transport, so it lives in `rustman-http`
//! (`rustman_http::runner`); this module holds everything that does not.

mod filter;
mod plan;
mod record;

pub use filter::{Filter, glob_matches};
pub use plan::{Plan, plan};
pub use record::{
    ExchangeRecord, HeaderRecord, RedirectRecord, RequestRecord, RequestStatus, ResponseRecord,
    RunRecord, SentRequest, SkipReason, Summary, TimingsRecord, now_rfc3339,
};
