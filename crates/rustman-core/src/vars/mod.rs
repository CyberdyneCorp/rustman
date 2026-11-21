//! Variables: `{{name}}` templates, dynamic values and the precedence chain
//! (design D8, environments spec).

pub mod dynamic;
pub mod graphql;
pub mod grpc;
pub mod jsonrpc;
pub mod mcp;
pub mod messaging;
pub mod placeholders;
pub mod realtime;
pub mod request;
pub mod resolver;
pub mod source;
pub mod template;

pub use dynamic::Dynamic;
pub use graphql::{ResolvedGraphql, resolve_graphql};
pub use grpc::{GRPC_SENT_METHOD, ResolvedGrpc, ResolvedSchema, resolve_grpc};
pub use jsonrpc::{ResolvedCall, ResolvedJsonrpc, resolve_jsonrpc};
pub use mcp::{ResolvedMcp, resolve_mcp};
pub use messaging::{
    ResolvedAmqp, ResolvedAsyncApi, ResolvedMessage, ResolvedMessaging, ResolvedMqtt,
    ResolvedPayload, ResolvedRegistry, resolve_messaging,
};
pub use placeholders::{PlaceholderVar, resolve_placeholders};
pub use realtime::{
    ResolvedBinary, ResolvedFrame, ResolvedRealtime, ResolvedSse, ResolvedStep, ResolvedWs,
    resolve_realtime,
};
pub use request::{
    ResolvedBody, ResolvedFile, ResolvedPair, ResolvedPart, ResolvedPartSource, ResolvedRequest,
    TlsFiles, resolve_auth, resolve_request, resolve_value,
};
pub use resolver::{
    Interpolator, Missing, ResolveError, Resolved, Resolver, SecretSpan, UsedSecret,
};
pub use source::{
    EnvironmentSource, Layer, Lookup, NoSecrets, SecretLookup, VarSource, VariablesSource,
    parse_overrides,
};
pub use template::{Segment, Template, TemplateError};
