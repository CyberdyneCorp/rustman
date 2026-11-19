//! gRPC statuses and their rich error details (spec "Status and error
//! details"). `grpc-status-details-bin` is decoded as `google.rpc.Status`
//! with a descriptor set of the `google/rpc` protos embedded at build time
//! (design D8); known detail types are rendered by type, unknown ones as
//! their type URL plus base64.

use std::fmt;
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use prost_reflect::{DescriptorPool, DynamicMessage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::json::to_json;

/// The `google/rpc` descriptor set compiled by `build.rs`.
static GOOGLE_RPC: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/google_rpc.bin"));

static POOL: LazyLock<DescriptorPool> = LazyLock::new(|| {
    DescriptorPool::decode(GOOGLE_RPC).expect("the embedded google/rpc descriptor set decodes")
});

/// The code of a successful call.
pub const OK: i32 = 0;
/// The code of a call cancelled by the client.
pub const CANCELLED: i32 = 1;
/// The code of an error without a more specific code.
pub const UNKNOWN: i32 = 2;
/// The code of a call whose deadline expired.
pub const DEADLINE_EXCEEDED: i32 = 4;
/// The code of a caller not allowed to make the call.
pub const PERMISSION_DENIED: i32 = 7;
/// The code of a call that exceeded a resource limit (a message too large).
pub const RESOURCE_EXHAUSTED: i32 = 8;
/// The code of a method the server does not implement.
pub const UNIMPLEMENTED: i32 = 12;
/// The code of an internal error.
pub const INTERNAL: i32 = 13;
/// The code of a server that is not available.
pub const UNAVAILABLE: i32 = 14;
/// The code of a caller without valid credentials.
pub const UNAUTHENTICATED: i32 = 16;

/// The name of status code `code`, such as `NOT_FOUND`; `UNKNOWN` for codes
/// outside the standard range.
pub fn code_name(code: i32) -> &'static str {
    rustman_core::model::grpc_code_name(code)
}

/// The code named `name` (case-insensitive), or a number.
pub fn parse_code(name: &str) -> Option<i32> {
    rustman_core::model::parse_grpc_code(name)
}

/// The final status of a call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GrpcStatus {
    pub code: i32,
    /// `OK`, `NOT_FOUND`, …
    pub name: String,
    pub message: String,
    /// Decoded from `grpc-status-details-bin`.
    pub details: Vec<StatusDetail>,
}

impl GrpcStatus {
    /// A status without details.
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            name: code_name(code).to_owned(),
            message: message.into(),
            details: Vec::new(),
        }
    }

    /// A status with the details of an encoded `google.rpc.Status`.
    pub fn with_details(code: i32, message: impl Into<String>, details: &[u8]) -> Self {
        Self {
            details: decode_details(details),
            ..Self::new(code, message)
        }
    }

    /// The status of a tonic call.
    pub fn from_tonic(status: &tonic::Status) -> Self {
        Self::with_details(status.code() as i32, status.message(), status.details())
    }

    pub fn is_ok(&self) -> bool {
        self.code == OK
    }

    /// `NOT_FOUND (5)`.
    pub fn label(&self) -> String {
        format!("{} ({})", self.name, self.code)
    }

    /// The status and its details as text: the label and message on the
    /// first line, one line per detail entry after it.
    pub fn render(&self) -> String {
        let mut text = self.label();
        if !self.message.is_empty() {
            text.push_str(": ");
            text.push_str(&self.message);
        }
        for line in self.details.iter().flat_map(StatusDetail::lines) {
            text.push_str("\n  ");
            text.push_str(&line);
        }
        text
    }
}

impl fmt::Display for GrpcStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

/// One entry of `google.rpc.Status.details`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusDetail {
    pub type_url: String,
    /// The detail as JSON when its type is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// The encoded detail in base64 when its type is unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base64: Option<String>,
}

impl StatusDetail {
    /// The message type: the type URL after its last `/`.
    pub fn type_name(&self) -> &str {
        self.type_url.rsplit('/').next().unwrap_or_default()
    }

    /// The detail as text lines, by type.
    pub fn lines(&self) -> Vec<String> {
        let Some(value) = &self.value else {
            let base64 = self.base64.as_deref().unwrap_or_default();
            return vec![format!("{}: {base64}", self.type_url)];
        };
        let short = self.type_name().trim_start_matches("google.rpc.");
        match short {
            "BadRequest" => each(value, "fieldViolations", |v| {
                format!(
                    "field violation: {}: {}",
                    s(v, "field"),
                    s(v, "description")
                )
            }),
            "QuotaFailure" => each(value, "violations", |v| {
                format!(
                    "quota violation: {}: {}",
                    s(v, "subject"),
                    s(v, "description")
                )
            }),
            "PreconditionFailure" => each(value, "violations", |v| {
                let (kind, subject) = (s(v, "type"), s(v, "subject"));
                format!(
                    "precondition violation: {kind} {subject}: {}",
                    s(v, "description")
                )
            }),
            "Help" => each(value, "links", |v| {
                format!("help: {} {}", s(v, "description"), s(v, "url"))
            }),
            _ => vec![single_line(short, value)],
        }
    }
}

/// The one-line rendering of detail types with a single entry.
fn single_line(short: &str, v: &Value) -> String {
    match short {
        "ErrorInfo" => {
            let mut line = format!(
                "error info: reason {}, domain {}",
                s(v, "reason"),
                s(v, "domain")
            );
            if let Some(Value::Object(meta)) = v.get("metadata") {
                for (k, value) in meta {
                    line.push_str(&format!(", {k}={}", value.as_str().unwrap_or_default()));
                }
            }
            line
        }
        "RetryInfo" => format!("retry after {}", s(v, "retryDelay")),
        "ResourceInfo" => format!(
            "resource: {} {} (owner {}): {}",
            s(v, "resourceType"),
            s(v, "resourceName"),
            s(v, "owner"),
            s(v, "description")
        ),
        "LocalizedMessage" => format!(
            "localized message ({}): {}",
            s(v, "locale"),
            s(v, "message")
        ),
        "RequestInfo" => format!(
            "request info: {} {}",
            s(v, "requestId"),
            s(v, "servingData")
        ),
        "DebugInfo" => {
            let stack: Vec<&str> = v
                .get("stackEntries")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            format!("debug info: {} [{}]", s(v, "detail"), stack.join(" | "))
        }
        _ => format!("{short}: {v}"),
    }
}

/// One line per entry of the list `key` of `value`.
fn each(value: &Value, key: &str, line: impl Fn(&Value) -> String) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| items.iter().map(line).collect())
        .unwrap_or_default()
}

/// The string field `key` of `value`, empty when absent.
fn s<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The details of an encoded `google.rpc.Status`; nothing when `bytes` is
/// empty or not a status.
pub fn decode_details(bytes: &[u8]) -> Vec<StatusDetail> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let Some(status) = POOL.get_message_by_name("google.rpc.Status") else {
        return Vec::new();
    };
    let Ok(status) = DynamicMessage::decode(status, bytes) else {
        return Vec::new();
    };
    let Some(details) = status.get_field_by_name("details") else {
        return Vec::new();
    };
    details
        .as_list()
        .unwrap_or_default()
        .iter()
        .filter_map(|any| any.as_message())
        .map(detail)
        .collect()
}

/// One `google.protobuf.Any`, decoded when its type is known.
fn detail(any: &DynamicMessage) -> StatusDetail {
    let type_url = any
        .get_field_by_name("type_url")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let bytes = any
        .get_field_by_name("value")
        .and_then(|v| v.as_bytes().cloned())
        .unwrap_or_default();
    let name = type_url.rsplit('/').next().unwrap_or_default();
    let decoded = POOL
        .get_message_by_name(name)
        .and_then(|desc| DynamicMessage::decode(desc, bytes.as_ref()).ok());
    match decoded {
        Some(message) => StatusDetail {
            type_url,
            value: Some(to_json(&message)),
            base64: None,
        },
        None => StatusDetail {
            type_url,
            value: None,
            base64: Some(STANDARD.encode(&bytes)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_have_names() {
        assert_eq!(code_name(0), "OK");
        assert_eq!(code_name(5), "NOT_FOUND");
        assert_eq!(code_name(16), "UNAUTHENTICATED");
        assert_eq!(code_name(99), "UNKNOWN");
        assert_eq!(parse_code("not_found"), Some(5));
        assert_eq!(parse_code("4"), Some(4));
        assert_eq!(parse_code("NOPE"), None);
        assert_eq!(GrpcStatus::new(4, "late").label(), "DEADLINE_EXCEEDED (4)");
    }

    #[test]
    fn garbage_details_are_ignored() {
        assert!(decode_details(b"").is_empty());
        assert!(decode_details(&[0xff, 0xff, 0xff]).is_empty());
    }

    #[test]
    fn every_known_type_renders() {
        let detail = |name: &str, value: Value| StatusDetail {
            type_url: format!("type.googleapis.com/google.rpc.{name}"),
            value: Some(value),
            base64: None,
        };
        let cases = [
            (
                detail("RetryInfo", serde_json::json!({"retryDelay": "1.500s"})),
                "retry after 1.500s",
            ),
            (
                detail(
                    "QuotaFailure",
                    serde_json::json!({"violations": [{"subject": "u", "description": "d"}]}),
                ),
                "quota violation: u: d",
            ),
            (
                detail(
                    "PreconditionFailure",
                    serde_json::json!({"violations": [{"type": "TOS", "subject": "s", "description": "d"}]}),
                ),
                "precondition violation: TOS s: d",
            ),
            (
                detail(
                    "ResourceInfo",
                    serde_json::json!({"resourceType": "book", "resourceName": "b1", "owner": "o", "description": "gone"}),
                ),
                "resource: book b1 (owner o): gone",
            ),
            (
                detail(
                    "Help",
                    serde_json::json!({"links": [{"description": "docs", "url": "https://h.example"}]}),
                ),
                "help: docs https://h.example",
            ),
            (
                detail(
                    "LocalizedMessage",
                    serde_json::json!({"locale": "en", "message": "hi"}),
                ),
                "localized message (en): hi",
            ),
            (
                detail(
                    "DebugInfo",
                    serde_json::json!({"stackEntries": ["a", "b"], "detail": "x"}),
                ),
                "debug info: x [a | b]",
            ),
        ];
        for (detail, line) in cases {
            assert_eq!(detail.lines(), [line]);
        }
    }
}
