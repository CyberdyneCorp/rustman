//! The gRPC client for Rustman (add-grpc-client): schema discovery through
//! server reflection (v1 with a v1alpha fallback), local `.proto` files
//! compiled in-process, or descriptor sets; a descriptor cache for offline
//! browsing; the service/method browser with message templates; and
//! unary and streaming calls with a dynamic codec, over h2c, TLS (mTLS
//! included) or gRPC-Web, with metadata, deadlines, headless stream bounds
//! and decoded rich error details; the runner executor and the standard
//! health check.
//!
//! The request-file model and its interpolation live in `rustman-core`
//! ([`rustman_core::model::GrpcRequest`],
//! [`rustman_core::vars::resolve_grpc`]).

pub mod adhoc;
pub mod agent;
pub mod browse;
pub mod call;
pub mod codec;
pub mod connect;
mod error;
pub mod exchange;
pub mod health;
pub mod interactive;
pub mod json;
pub mod metadata;
pub mod run;
pub mod schema;
pub mod status;
pub mod target;
pub mod template;
pub mod web;

pub use adhoc::{AD_HOC, ad_hoc_entry, target_entry};
pub use browse::{
    CallType, Description, FieldInfo, MessageInfo, MethodInfo, ServiceInfo, has_no_side_effects,
};
pub use call::{
    CallOptions, DEFAULT_DEADLINE_MS, LiveFeed, PreparedCall, StreamBounds, effective_deadline,
};
pub use codec::DynamicCodec;
pub use connect::{
    ConnectTimings, Connection, DEFAULT_CONNECT_TIMEOUT, DEFAULT_SCHEMA_TIMEOUT, Remote,
};
pub use error::{EXIT_CONFIG, EXIT_REQUEST, GrpcError};
pub use exchange::{Direction, GrpcExchange, GrpcTimings, StreamStop, TimelineEntry};
pub use health::{HealthCheck, ServingStatus, check_health};
pub use interactive::Interactive;
pub use json::{to_json, to_message};
pub use metadata::MetadataEntry;
pub use run::{GrpcRunner, remote, schema_for};
pub use schema::{
    CacheMeta, CachePolicy, DESCRIPTOR_CACHE_DIR, DescriptorCache, Schema, SchemaOrigin,
    SchemaSource, compile_protos, load_descriptor_set, load_schema, reflect, reflect_schema,
};
pub use status::{GrpcStatus, StatusDetail, code_name, parse_code};
pub use target::{GrpcTarget, TargetScheme};
pub use template::{TEMPLATE_DEPTH, template};
