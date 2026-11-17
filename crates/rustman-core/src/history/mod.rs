//! Request history (request-history spec, design D5): a local, redacted
//! record of past exchanges that `rustman history` lists, shows and
//! replays, and that later front-ends (TUI, MCP) read the same way.
//!
//! Recording happens in the runner (`rustman_http::runner`), which knows the
//! resolved request and the response; this module holds the storage, the
//! filters and the replayable form of a request.

mod data_dir;
mod entry;
mod query;
mod replay;
mod store;
mod template;
mod ulid;

pub use data_dir::{DATA_DIR_VAR, rustman_data_dir};
pub use entry::{AD_HOC_REQUEST, ENTRY_SCHEMA, Entry, NewEntry};
pub use query::{Query, StatusFilter, parse_age};
pub use replay::{
    ReplayAuth, ReplayBody, ReplayFile, ReplayPair, ReplayRequest, ReplaySettings, ReplayTls,
};
pub use store::{HISTORY_DIR, HistoryError, HistoryStore, Listing};
pub use ulid::{new_ulid, timestamp_millis};
