//! WebSocket and Server-Sent Events sessions for Rustman
//! (add-realtime-client): the WebSocket handshake over `rustman-http`'s
//! shared helper (TLS, proxies, auth), scripted steps, ping/pong and close
//! handling, SSE streams with resume and reconnect, a capped message
//! timeline and headless bounds.
//!
//! The request-file models and their interpolation live in `rustman-core`
//! ([`rustman_core::model::WsRequest`], [`rustman_core::model::SseRequest`],
//! [`rustman_core::vars::resolve_realtime`]).

mod adhoc;
mod error;
mod exchange;
mod feed;
mod record;
pub mod run;
mod session;
mod sse;
mod timeline;
pub mod ws;

pub use adhoc::{AD_HOC, ad_hoc_entry, header_fields, kind_of};
pub use error::RealtimeError;
pub use exchange::{Handshake, RealtimeExchange, SessionEnd};
pub use feed::{
    FEED_MAX_BYTES, FEED_QUEUE, FeedItem, FeedReceiver, FeedSender, feed_channel, feed_channel_with,
};
pub use record::{messages, outcome, record};
pub use run::RealtimeRunner;
pub use session::{Bounds, COMMAND_QUEUE, Command, SessionOptions, command_channel};
pub use sse::run_sse;
pub use timeline::{Direction, Timeline, TimelineEntry, is_message, message_json};
pub use ws::{CLOSE_WAIT, run_ws};
