//! The typed request model, loaded through [`crate::format`].

pub mod assertion;
pub mod auth;
pub mod body;
pub mod capture;
pub mod common;
pub mod docs;
pub mod evm;
pub mod evm_check;
pub mod graphql;
pub mod grpc;
pub mod http;
pub mod jsonrpc;
pub mod kind;
pub mod mcp;
pub mod messaging;
pub mod signed_auth;
pub mod snapshot;
pub mod sse;
pub mod ws;

pub use assertion::{
    Assertion, BodyAssertion, CountExpectation, CountOp, GrpcStatusExpectation, HeaderAssertion,
    JsonAssertion, LatencyAssertion, MessagesAssertion, MetadataAssertion, PluginAssertion,
    RpcErrorExpectation, SchemaAssertion, StatusExpectation, SubgraphAssertion,
};
pub use auth::{ApiKeyLocation, Auth, OAuth2, OAuth2Grant, PluginAuth};
pub use body::{Body, JsonContent, Part, PartSource};
pub use capture::{Capture, CaptureSource, EVM_RECEIPT_FIELDS};
pub use common::{Checked, NameValue, Validate, VarValue, Variables};
pub use docs::DocsMeta;
pub use evm::{
    AbiSpec, DEPLOYMENT_OUT_OF_SCOPE, EVM_CALL_KIND, EVM_TX_KIND, EvmCallRequest, EvmCommon,
    EvmRequest, EvmTxRequest, SIGNING_UNAVAILABLE, Scalar, StateOverride, TxType,
};
pub use evm_check::{EVM_OUTPUT_OPS, EvmAssertion, OutputRef};
pub use graphql::{
    DEFAULT_DURATION, DEFAULT_MAX_EVENTS, DOCUMENT_SOURCE_ERROR, DocumentSource, GRAPHQL_ACCEPT,
    GraphqlMethod, GraphqlPreset, GraphqlRequest, GraphqlTransport, WsProtocol, graphql_as_http,
};
pub use grpc::{
    BOTH_MESSAGES_ERROR, DEFAULT_STREAM_DURATION_MS, DEFAULT_STREAM_MESSAGES, GRPC_CODE_NAMES,
    GrpcRequest, GrpcSchema, GrpcTls, GrpcTransport, NO_MESSAGE_ERROR, StreamLimits,
    grpc_code_name, parse_grpc_code,
};
pub use http::{HttpRequest, Method, RequestSettings, Transform};
pub use jsonrpc::{
    BatchCall, CallFields, EndpointScheme, JsonrpcRequest, JsonrpcSettings, MAX_BATCH_CALLS,
    RetrySettings, SubscriptionSpec, jsonrpc_as_http, unlocated,
};
pub use kind::{
    BUILTIN_KINDS, COMMON_FIELDS, KindLoader, KindRegistry, OpaqueRequest, REQUEST_VERSION, Request,
};
pub use mcp::{McpOperation, McpRequest};
pub use messaging::{
    AckMode, Acks, AsyncApiRef, BrokerAuth, Brokers, KafkaProtocol, KafkaSecurity,
    MESSAGING_DEFAULT_DURATION_MS, MESSAGING_DEFAULT_MAX_MESSAGE_BYTES,
    MESSAGING_DEFAULT_MAX_MESSAGES, Message, MessageHeader, MessageProperties, MessagingKind,
    MessagingLimits, MessagingRequest, MqttVersion, NAMED_QUEUE_ERROR, OPERATION_ERROR, Operation,
    Payload, Qos, SaslMechanism, SchemaRegistry, SchemaVersion, Start, StartOffset,
};
pub use signed_auth::{AwsCredentials, AwsSigV4, Jwt, JwtAlgorithm};
pub use snapshot::{
    ComparedPart, DEFAULT_SNAPSHOT_NAME, Preset, Replacement, SnapshotAssertion, SnapshotHeaders,
    SortRule,
};
pub use sse::{
    DEFAULT_MAX_ATTEMPTS, DEFAULT_RETRY_MS, MAX_RETRY_MS, Reconnect, SSE_ACCEPT, SSE_SCHEME_ERROR,
    SseRequest,
};
pub use ws::{
    BinarySource, CloseStep, DEFAULT_DURATION_MS, DEFAULT_MAX_MESSAGE_BYTES, DEFAULT_MAX_MESSAGES,
    DEFAULT_MAX_TIMELINE, Frame, MAX_CLOSE_REASON, RealtimeLimits, RealtimeTls, Step, WaitFor,
    WsRequest,
};

/// Request files end with this suffix.
pub const REQUEST_FILE_SUFFIX: &str = ".rman.yaml";
