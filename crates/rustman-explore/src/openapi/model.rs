//! `ApiModel`: the normalized IR every stage after import consumes (design
//! D1). OpenAPI 3.0 and 3.1 documents lower to the same model; schemas are
//! kept as raw 2020-12 JSON Schema with `$ref`s into [`ApiModel::components`].

use serde_json::Value;

/// One API, as described by its document.
#[derive(Clone, Debug, PartialEq)]
pub struct ApiModel {
    pub title: String,
    /// `info.version`: the API's version, not the OpenAPI version.
    pub version: String,
    pub description: Option<String>,
    pub servers: Vec<Server>,
    /// Declared tags, in document order.
    pub tags: Vec<Tag>,
    /// Operations in document order (paths, then methods in the order
    /// get, put, post, delete, options, head, patch, trace).
    pub operations: Vec<Operation>,
    /// The document-wide security requirements.
    pub security: Vec<SecurityRequirement>,
    /// `components.securitySchemes`, in document order.
    pub security_schemes: Vec<(String, SecurityScheme)>,
    /// The raw `components` object: what every schema `$ref` points into.
    pub components: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Server {
    pub url: String,
    pub description: Option<String>,
    pub variables: Vec<ServerVariable>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerVariable {
    pub name: String,
    pub default: String,
    pub values: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tag {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Operation {
    /// The JSON pointer of the operation, e.g. `/paths/~1pets/get`.
    pub pointer: String,
    pub operation_id: Option<String>,
    /// Upper case: `GET`, `POST`, ...
    pub method: String,
    /// The path template, e.g. `/pets/{petId}`.
    pub path: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    /// Path-level and operation-level parameters, the operation's winning.
    pub parameters: Vec<Parameter>,
    pub request_body: Option<RequestBody>,
    /// Responses in document order, keyed by status (`200`, `4XX`, `default`).
    pub responses: Vec<Response>,
    /// `None` inherits [`ApiModel::security`]; `Some(vec![])` is public.
    pub security: Option<Vec<SecurityRequirement>>,
    pub deprecated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterLocation {
    Path,
    Query,
    Header,
    Cookie,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Parameter {
    pub name: String,
    pub location: ParameterLocation,
    pub required: bool,
    pub schema: Option<Value>,
    pub example: Option<Value>,
    pub deprecated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RequestBody {
    pub required: bool,
    pub content: Vec<MediaType>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Response {
    pub status: String,
    pub description: Option<String>,
    pub content: Vec<MediaType>,
    /// Declared response header names.
    pub headers: Vec<String>,
    /// `links`, in document order.
    pub links: Vec<Link>,
}

/// A response `link`: how values of this response feed another operation.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub name: String,
    /// The target's `operationId`.
    pub operation_id: Option<String>,
    /// The target as a reference, e.g. `#/paths/~1users~1{id}/get`.
    pub operation_ref: Option<String>,
    /// Target parameter names and their values (runtime expressions such
    /// as `$response.body#/id`, or constants).
    pub parameters: Vec<(String, Value)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MediaType {
    /// The media type range, e.g. `application/json`.
    pub media_type: String,
    pub schema: Option<Value>,
    pub example: Option<Value>,
    /// Named examples in document order (`value` of each Example object).
    pub examples: Vec<(String, Value)>,
}

/// Alternatives: any one requirement satisfied is enough. Each requirement
/// lists scheme names with their scopes, all needed together.
pub type SecurityRequirement = Vec<(String, Vec<String>)>;

#[derive(Clone, Debug, PartialEq)]
pub enum SecurityScheme {
    HttpBasic,
    HttpBearer {
        format: Option<String>,
    },
    /// Another `http` scheme, such as `digest`.
    Http {
        scheme: String,
    },
    ApiKey {
        name: String,
        location: ParameterLocation,
    },
    OAuth2 {
        flows: Value,
    },
    OpenIdConnect {
        url: String,
    },
    MutualTls,
}

impl Operation {
    /// The parameters at `location`.
    pub fn parameters_in(&self, location: ParameterLocation) -> impl Iterator<Item = &Parameter> {
        self.parameters
            .iter()
            .filter(move |p| p.location == location)
    }
}
