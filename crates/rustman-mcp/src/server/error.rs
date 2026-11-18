//! Tool errors: a machine-readable `code` from a fixed catalog plus a
//! message, returned as tool results with `isError: true` rather than as
//! protocol errors (spec "Structured tools with schemas").

use std::fmt;

use rmcp::model::CallToolResult;
use serde::Serialize;
use serde_json::json;

/// Every error code a tool can return; documented in `docs/agent.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// A request, environment, report or resource that does not exist.
    NotFound,
    /// Arguments that are malformed, or a result too large to return.
    InvalidInput,
    /// Refused by the safety policy or the agent permissions.
    Blocked,
    /// The human declined (or dismissed) the confirmation.
    Declined,
    /// A mutating call needs confirmation and the client cannot ask.
    ConfirmationUnavailable,
    /// Too many background jobs are running.
    Busy,
    /// Anything else, such as the audit log not being writable.
    Internal,
}

impl ErrorCode {
    /// Every code, in catalog order.
    pub const ALL: [ErrorCode; 7] = [
        Self::NotFound,
        Self::InvalidInput,
        Self::Blocked,
        Self::Declined,
        Self::ConfirmationUnavailable,
        Self::Busy,
        Self::Internal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::InvalidInput => "invalid_input",
            Self::Blocked => "blocked",
            Self::Declined => "declined",
            Self::ConfirmationUnavailable => "confirmation_unavailable",
            Self::Busy => "busy",
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A tool call that failed for a reason the agent can act on.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ToolError {
    pub code: ErrorCode,
    pub message: String,
}

impl ToolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn blocked(reason: impl Into<String>) -> Self {
        Self::new(ErrorCode::Blocked, reason)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// The JSON the agent receives as `structuredContent`.
    pub fn to_json(&self) -> serde_json::Value {
        json!({ "code": self.code, "message": self.message })
    }

    /// The tool result: `isError: true`, the code and message as
    /// `structuredContent` and a one-line summary as text.
    pub fn into_result(self) -> CallToolResult {
        let mut result = CallToolResult::structured_error(self.to_json());
        result.content = vec![rmcp::model::ContentBlock::text(self.to_string())];
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_serialize_as_their_catalog_names() {
        for code in ErrorCode::ALL {
            let json = serde_json::to_value(code).unwrap();
            assert_eq!(json, code.as_str());
        }
    }

    #[test]
    fn an_error_is_a_tool_result_with_a_code() {
        let result = ToolError::blocked("host not allowed").into_result();
        assert_eq!(result.is_error, Some(true));
        let structured = result.structured_content.unwrap();
        assert_eq!(structured["code"], "blocked");
        assert_eq!(structured["message"], "host not allowed");
        let text = serde_json::to_value(&result.content[0]).unwrap();
        assert_eq!(text["text"], "blocked: host not allowed");
    }
}
