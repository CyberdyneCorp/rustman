//! The GraphQL client for Rustman (add-graphql-client): `kind: graphql`
//! documents, schemas from introspection, the on-disk schema cache and
//! `schema_file`, pre-send validation, explorer and completion data, and
//! execution over HTTP (JSON, GET, multipart uploads, APQ), subscriptions
//! over WebSocket and SSE, and the executor `rustman run` uses.
//!
//! The request-file model and its interpolation live in `rustman-core`
//! ([`rustman_core::model::GraphqlRequest`],
//! [`rustman_core::vars::resolve_graphql`]); HTTP goes through
//! `rustman-http`.

pub mod cache;
pub mod completion;
mod deprecation;
pub mod document;
mod error;
pub mod execute;
pub mod explorer;
pub mod fetch;
pub mod highlight;
pub mod introspection;
pub mod operation;
pub mod paginate;
pub mod preset;
pub mod response;
pub mod run;
pub mod schema;
pub mod source;
pub mod subscription;
pub mod upload;
pub mod validate;
mod variables;

pub use cache::{CachedSchema, SCHEMA_CACHE_DIR, SchemaCache, cache_key, normalize_endpoint};
pub use completion::{Completion, CompletionKind, complete};
pub use deprecation::deprecation_reason;
pub use document::load_document;
pub use error::GraphqlError;
pub use execute::{ApqOutcome, HttpOperation, HttpResult, execute_http};
pub use explorer::{
    DirectiveInfo, EnumValueInfo, FieldInfo, InputValueInfo, SchemaOverview, TypeDetail, TypeKind,
    TypeSummary,
};
pub use fetch::{Introspected, introspect, refresh_schema};
pub use highlight::{Highlight, TokenClass, highlight};
pub use operation::{OperationKind, SelectedOperation, check_http_method, select_operation};
pub use paginate::{DEFAULT_MAX_PAGES, MAX_PAGED_BYTES, MAX_PAGES_LIMIT, PageMode, Paginate};
pub use response::{GraphqlResponse, NOT_GRAPHQL, ResponseError, ResultStatus};
pub use run::{
    ChainHead, Delivery, Executed, Execution, GraphqlRunner, Interactive, META_QUERY, execute,
    execute_paged,
};
pub use schema::GraphqlSchema;
pub use source::{ActiveSchema, SchemaOrigin, active_schema};
pub use subscription::{
    DEFAULT_ACK_TIMEOUT, Ended, Limits, SubscribeOptions, Subscription, SubscriptionEvent,
    SubscriptionProtocol, WireProtocol, subscribe_sse, subscribe_ws,
};
pub use upload::{Upload, extract_uploads};
pub use validate::{Diagnostic, Position, Severity, Validation, validate};
