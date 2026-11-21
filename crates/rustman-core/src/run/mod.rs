//! Running collections (cli-runner spec "Run collections", design D10): which
//! requests a run executes and in what units ([`plan`]), and what each
//! request produced ([`record`]), already redacted for every output sink.
//!
//! Executing a plan needs the transport, so it lives in `rustman-http`
//! (`rustman_http::runner`); this module holds everything that does not.

mod data;
mod filter;
mod graphql;
mod jsonrpc;
mod mcp;
mod messaging;
mod plan;
mod plugin;
mod realtime;
mod record;

pub use data::{CollectionData, DataRun, collection_data, secret_columns};
pub use filter::{Filter, glob_matches};
pub use graphql::{
    EventRecord, GraphqlRecord, LAG_NO_RPC, MAX_RECORDED_EVENTS, PresetRecord, SubgraphFacts,
    SubscriptionRecord, redact_value,
};
pub use jsonrpc::{
    JsonrpcRecord, MAX_RPC_EVENTS, RpcAttemptRecord, RpcCallRecord, RpcRefusal,
    RpcSubscriptionRecord,
};
pub use mcp::{MAX_RECORDED_NOTIFICATIONS, McpRecord};
pub use messaging::MessagingRecord;
pub use plan::{Plan, plan};
pub use plugin::{PluginKindRecord, TransformRecord};
pub use realtime::{
    EndRecord, HandshakeRecord, MAX_RECORDED_FIELD, MAX_RECORDED_PAYLOAD, RealtimeRecord,
    TimelineRecord,
};
pub use record::{
    DataRowRecord, ExchangeRecord, HeaderRecord, RedirectRecord, RequestRecord, RequestStatus,
    ResponseRecord, RunRecord, SentRequest, SkipReason, Summary, TimingsRecord, now_rfc3339,
};
