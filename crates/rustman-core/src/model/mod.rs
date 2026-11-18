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

pub use assertion::{
    Assertion, BodyAssertion, HeaderAssertion, JsonAssertion, LatencyAssertion, SchemaAssertion,
    StatusExpectation,
};
pub use auth::{ApiKeyLocation, Auth, OAuth2, OAuth2Grant};
pub use body::{Body, JsonContent, Part, PartSource};
pub use capture::{Capture, CaptureSource};
pub use common::{Checked, NameValue, Validate, VarValue, Variables};
pub use graphql::{GRAPHQL_ACCEPT, graphql_as_http};
pub use http::{HttpRequest, Method, RequestSettings};
pub use kind::{KindLoader, KindRegistry, OpaqueRequest, REQUEST_VERSION, Request};
pub use signed_auth::{AwsCredentials, AwsSigV4, Jwt, JwtAlgorithm};

/// Request files end with this suffix.
pub const REQUEST_FILE_SUFFIX: &str = ".rman.yaml";
