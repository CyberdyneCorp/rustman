//! The typed request model, loaded through [`crate::format`].

pub mod assertion;
pub mod auth;
pub mod body;
pub mod capture;
pub mod common;
pub mod graphql;
pub mod http;
pub mod kind;
pub mod signed_auth;
pub mod snapshot;

pub use assertion::{
    Assertion, BodyAssertion, CountExpectation, CountOp, HeaderAssertion, JsonAssertion,
    LatencyAssertion, SchemaAssertion, StatusExpectation,
};
pub use auth::{ApiKeyLocation, Auth, OAuth2, OAuth2Grant};
pub use body::{Body, JsonContent, Part, PartSource};
pub use capture::{Capture, CaptureSource};
pub use common::{Checked, NameValue, Validate, VarValue, Variables};
pub use graphql::{
    DEFAULT_DURATION, DEFAULT_MAX_EVENTS, DOCUMENT_SOURCE_ERROR, DocumentSource, GRAPHQL_ACCEPT,
    GraphqlMethod, GraphqlRequest, GraphqlTransport, WsProtocol, graphql_as_http,
};
pub use http::{HttpRequest, Method, RequestSettings};
pub use kind::{KindLoader, KindRegistry, OpaqueRequest, REQUEST_VERSION, Request};
pub use signed_auth::{AwsCredentials, AwsSigV4, Jwt, JwtAlgorithm};
pub use snapshot::{
    ComparedPart, DEFAULT_SNAPSHOT_NAME, Preset, Replacement, SnapshotAssertion, SnapshotHeaders,
    SortRule,
};

/// Request files end with this suffix.
pub const REQUEST_FILE_SUFFIX: &str = ".rman.yaml";
