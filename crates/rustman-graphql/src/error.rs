//! Errors of the GraphQL client. Messages start with `graphql:` and never
//! contain unredacted secrets.

use rustman_core::Redactor;
use rustman_core::paths::PathEscape;
use rustman_core::vars::ResolveError;
use rustman_http::RequestError;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GraphqlError {
    /// The server did not answer the introspection query with a schema.
    #[error("graphql: introspection failed: {0}; set schema_file to validate offline")]
    IntrospectionFailed(String),
    /// An introspection result that cannot be turned into a schema.
    #[error("graphql: invalid introspection result: {0}")]
    InvalidIntrospection(String),
    /// SDL that does not parse, from `origin` (a file or an endpoint).
    #[error("graphql: invalid schema {origin}: {message}")]
    InvalidSchema { origin: String, message: String },
    #[error("graphql: cannot read {path}: {message}")]
    Read { path: String, message: String },
    #[error("graphql: cannot write {path}: {message}")]
    Write { path: String, message: String },
    #[error("graphql: invalid endpoint URL '{0}'")]
    InvalidEndpoint(String),
    #[error("graphql: {0}")]
    PathEscape(String),
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    /// Several operations and no `operation_name` to pick one.
    #[error("graphql: document has {0} operations; set operation_name")]
    AmbiguousOperation(usize),
    #[error("graphql: operation '{0}' is not in the document")]
    UnknownOperation(String),
    #[error("graphql: the document has no operation")]
    NoOperation,
    #[error("graphql: mutations cannot use GET")]
    MutationOverGet,
    #[error("graphql: subscriptions cannot be sent over HTTP; use transport ws or sse")]
    SubscriptionOverHttp,
    #[error("graphql: uploads cannot use GET")]
    UploadOverGet,
    #[error("graphql: upload file not found: {0}")]
    UploadNotFound(String),
    /// Validation errors that block the send, one `line:column: message`
    /// each.
    #[error("graphql: the document is invalid: {}", .0.join("; "))]
    Invalid(Vec<String>),
    /// A WebSocket server that never acknowledged `connection_init`.
    #[error("graphql: connection_ack timeout")]
    AckTimeout,
    /// A subscription that could not start or broke off; redacted.
    #[error("graphql: subscription: {0}")]
    Subscription(String),
    /// A response body larger than `[http] max_body_bytes`, refused unread:
    /// neither parsed nor recorded.
    #[error("HTTP: the response body is larger than the {0} byte limit (max_body_bytes)")]
    BodyTooLarge(u64),
    /// `--paginate` cannot page this operation.
    #[error("graphql: --paginate: {0}")]
    Paginate(String),
    /// The transport failed; the message is redacted.
    #[error(transparent)]
    Transport(#[from] RequestError),
}

impl GraphqlError {
    /// The same error with every secret `redactor` knows replaced in the
    /// messages that can quote what was sent.
    #[must_use]
    pub fn redacted(self, redactor: &Redactor) -> Self {
        match self {
            Self::Subscription(m) => Self::Subscription(redactor.text(&m).into_owned()),
            Self::Transport(e) => Self::Transport(e.redacted(redactor)),
            other => other,
        }
    }
}

impl From<PathEscape> for GraphqlError {
    fn from(err: PathEscape) -> Self {
        Self::PathEscape(err.to_string())
    }
}
