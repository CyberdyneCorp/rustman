//! Errors of the gRPC client. Messages follow the spec texts and never
//! contain unredacted secrets once passed through [`GrpcError::redacted`].

use rustman_core::Redactor;

/// The exit code of a usage or configuration error (`cli-runner`).
pub const EXIT_CONFIG: i32 = 2;

/// The exit code of a request error: network, TLS, timeout (`cli-runner`).
pub const EXIT_REQUEST: i32 = 3;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GrpcError {
    /// A target that is not `host:port` or a `grpc://`/`grpcs://` URL.
    #[error("grpc: invalid target '{0}': expected host:port or a grpc:// or grpcs:// URL")]
    Target(String),
    /// The server could not be reached.
    #[error("grpc: cannot connect to {target}: {message}")]
    Connect { target: String, message: String },
    /// Neither reflection version is implemented and no schema is set.
    #[error(
        "grpc: server reflection unavailable at {0}; set schema.protos or schema.descriptor_set"
    )]
    ReflectionUnavailable(String),
    /// Reflection answered with an error or an unusable descriptor.
    #[error("grpc: reflection at {target} failed: {message}")]
    Reflection { target: String, message: String },
    /// A `.proto` compile error: `<file>:<line>:<col>: <message>`.
    #[error("{0}")]
    Compile(String),
    /// A descriptor set or cache file that cannot be read or decoded.
    #[error("grpc: {0}")]
    Schema(String),
    /// A service, method or message the schema does not define.
    #[error("grpc: unknown symbol '{0}'")]
    UnknownSymbol(String),
    /// Something this build cannot do yet.
    #[error("grpc: {0}")]
    Unsupported(String),
    /// A request message names a field its type does not have.
    #[error("grpc: unknown field '{field}' in {message_type}")]
    UnknownField { field: String, message_type: String },
    /// A request message that does not fit its type, or a method called
    /// with the wrong number of messages.
    #[error("grpc: {0}")]
    Message(String),
    /// Request metadata that cannot be sent (bad name, bad base64).
    #[error("grpc: invalid metadata '{name}': {message}")]
    Metadata { name: String, message: String },
    /// The TLS handshake failed, certificate verification included.
    #[error("grpc: TLS error at {target}: {message}")]
    Tls { target: String, message: String },
    /// The connection failed during the call (not a gRPC status).
    #[error("grpc: call to {target} failed: {message}")]
    Transport { target: String, message: String },
}

impl GrpcError {
    /// The `cli-runner` exit code for this error: 3 when the server could
    /// not be reached, 2 for every schema or configuration problem.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Connect { .. }
            | Self::Reflection { .. }
            | Self::Tls { .. }
            | Self::Transport { .. } => EXIT_REQUEST,
            _ => EXIT_CONFIG,
        }
    }

    /// Whether the server could not be reached at all (a cached schema
    /// may stand in).
    pub fn is_unreachable(&self) -> bool {
        matches!(self, Self::Connect { .. })
    }

    /// The same error with every secret `redactor` knows replaced.
    #[must_use]
    pub fn redacted(self, redactor: &Redactor) -> Self {
        let clean = |text: String| redactor.text(&text).into_owned();
        match self {
            Self::Target(t) => Self::Target(clean(t)),
            Self::Connect { target, message } => Self::Connect {
                target: clean(target),
                message: clean(message),
            },
            Self::ReflectionUnavailable(t) => Self::ReflectionUnavailable(clean(t)),
            Self::Reflection { target, message } => Self::Reflection {
                target: clean(target),
                message: clean(message),
            },
            Self::Compile(m) => Self::Compile(clean(m)),
            Self::Schema(m) => Self::Schema(clean(m)),
            Self::UnknownSymbol(s) => Self::UnknownSymbol(clean(s)),
            Self::Unsupported(m) => Self::Unsupported(clean(m)),
            Self::UnknownField {
                field,
                message_type,
            } => Self::UnknownField {
                field: clean(field),
                message_type,
            },
            Self::Message(m) => Self::Message(clean(m)),
            Self::Metadata { name, message } => Self::Metadata {
                name: clean(name),
                message: clean(message),
            },
            Self::Tls { target, message } => Self::Tls {
                target: clean(target),
                message: clean(message),
            },
            Self::Transport { target, message } => Self::Transport {
                target: clean(target),
                message: clean(message),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::Secret;

    #[test]
    fn reflection_unavailable_follows_the_spec() {
        let err = GrpcError::ReflectionUnavailable("grpc://h:1".into());
        assert_eq!(
            err.to_string(),
            "grpc: server reflection unavailable at grpc://h:1; set schema.protos or schema.descriptor_set"
        );
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn exit_codes() {
        let connect = GrpcError::Connect {
            target: "h:1".into(),
            message: "refused".into(),
        };
        assert_eq!(connect.exit_code(), 3);
        assert!(connect.is_unreachable());
        assert_eq!(GrpcError::Compile("a.proto:1:1: x".into()).exit_code(), 2);
        assert_eq!(GrpcError::UnknownSymbol("x".into()).exit_code(), 2);
        let tls = GrpcError::Tls {
            target: "grpcs://h:1".into(),
            message: "invalid peer certificate".into(),
        };
        assert_eq!(tls.exit_code(), 3);
        assert_eq!(
            tls.to_string(),
            "grpc: TLS error at grpcs://h:1: invalid peer certificate"
        );
        let unknown = GrpcError::UnknownField {
            field: "idd".into(),
            message_type: "shop.v1.GetProductRequest".into(),
        };
        assert_eq!(unknown.exit_code(), 2);
    }

    #[test]
    fn messages_are_redacted() {
        let redactor = Redactor::new().with(&Secret::new("host", "s3cr3t-host".to_owned()));
        let err = GrpcError::Connect {
            target: "s3cr3t-host:1".into(),
            message: "refused by s3cr3t-host".into(),
        }
        .redacted(&redactor);
        assert!(!err.to_string().contains("s3cr3t"), "{err}");
        assert!(err.to_string().contains("«redacted:host»"), "{err}");
    }
}
