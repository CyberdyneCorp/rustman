//! One MCP operation as request files, `rustman mcp-client` and the TUI
//! name it (mcp-client "MCP request files"): `tools/list`, `tools/call`,
//! `resources/list`, `resources/read`, `prompts/list` and `prompts/get`,
//! performed on a session and answered as the MCP result JSON that
//! assertions read, plus what the result view and history need.

use std::collections::BTreeMap;
use std::time::Duration;

use rustman_core::model::McpOperation;
use serde_json::{Map, Value, json};

use super::error::ClientError;
use super::notify::Notification;
use super::output;
use super::resources::ResourceContent;
use super::schema::Violation;
use super::session::McpSession;
use super::tools::{CallOptions, ToolResult};

/// An operation with its params.
#[derive(Clone, Debug, PartialEq)]
pub enum McpOp {
    ToolsList,
    ToolsCall {
        name: String,
        arguments: Value,
    },
    ResourcesList,
    ResourcesRead {
        uri: String,
    },
    PromptsList,
    PromptsGet {
        name: String,
        arguments: BTreeMap<String, String>,
    },
}

/// What an operation answered.
#[derive(Clone, Debug)]
pub struct OpResult {
    /// The MCP result, as the server shapes it (`tools`, `content`,
    /// `structuredContent`, `isError`, `contents`, `messages`...).
    pub result: Value,
    /// `ok`, or `tool error` for an `isError` tool result.
    pub outcome: &'static str,
    /// A `tools/call` result with `isError: true`.
    pub is_error: bool,
    /// The violations of the tool's `outputSchema`; `None` when it declares
    /// none (or the operation is not `tools/call`).
    pub output_violations: Option<Vec<Violation>>,
    /// Input violations sent anyway (`--no-validate`, `validate: false`).
    pub overridden: Vec<Violation>,
    /// What captures read: `structuredContent`, else the text content.
    pub capture_body: String,
    /// The result as a person reads it (see [`super::output`]).
    pub text: String,
    /// The notifications received while it ran.
    pub notifications: Vec<Notification>,
    pub elapsed: Duration,
}

impl McpOp {
    /// The operation of a request file from its (resolved) params.
    pub fn from_params(
        operation: McpOperation,
        params: &Map<String, Value>,
    ) -> Result<Self, ClientError> {
        let text = |key: &str| {
            params
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| ClientError::Invalid(format!("{operation} requires params.{key}")))
        };
        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
        Ok(match operation {
            McpOperation::ToolsList => Self::ToolsList,
            McpOperation::ToolsCall => Self::ToolsCall {
                name: text("name")?,
                arguments: match arguments {
                    Value::Null => Value::Object(Map::new()),
                    other => other,
                },
            },
            McpOperation::ResourcesList => Self::ResourcesList,
            McpOperation::ResourcesRead => Self::ResourcesRead { uri: text("uri")? },
            McpOperation::PromptsList => Self::PromptsList,
            McpOperation::PromptsGet => Self::PromptsGet {
                name: text("name")?,
                arguments: string_arguments(&arguments),
            },
        })
    }

    pub fn operation(&self) -> McpOperation {
        match self {
            Self::ToolsList => McpOperation::ToolsList,
            Self::ToolsCall { .. } => McpOperation::ToolsCall,
            Self::ResourcesList => McpOperation::ResourcesList,
            Self::ResourcesRead { .. } => McpOperation::ResourcesRead,
            Self::PromptsList => McpOperation::PromptsList,
            Self::PromptsGet { .. } => McpOperation::PromptsGet,
        }
    }

    /// The params as a request file writes them.
    pub fn params(&self) -> Map<String, Value> {
        let value = match self {
            Self::ToolsList | Self::ResourcesList | Self::PromptsList => json!({}),
            Self::ToolsCall { name, arguments } => json!({"name": name, "arguments": arguments}),
            Self::ResourcesRead { uri } => json!({"uri": uri}),
            Self::PromptsGet { name, arguments } => json!({"name": name, "arguments": arguments}),
        };
        match value {
            Value::Object(map) => map,
            _ => Map::new(),
        }
    }
}

/// Prompt arguments are strings: other JSON values are sent as their text.
fn string_arguments(arguments: &Value) -> BTreeMap<String, String> {
    let Value::Object(map) = arguments else {
        return BTreeMap::new();
    };
    map.iter()
        .map(|(k, v)| {
            let text = v.as_str().map_or_else(|| v.to_string(), str::to_owned);
            (k.clone(), text)
        })
        .collect()
}

impl McpSession {
    /// Perform `op`. Tool arguments are validated unless `options` says not.
    pub async fn perform(
        &self,
        op: &McpOp,
        options: &CallOptions,
    ) -> Result<OpResult, ClientError> {
        let request = &options.request;
        match op {
            McpOp::ToolsCall { name, arguments } => {
                self.perform_call(name, arguments, options).await
            }
            McpOp::ToolsList => {
                let e = self.list_tools(request).await?;
                let result = json!({"tools": to_json(&*e.result)});
                Ok(plain(
                    result,
                    output::tools(&e.result),
                    e.notifications,
                    e.elapsed,
                ))
            }
            McpOp::ResourcesList => {
                let e = self.list_resources(request).await?;
                let result = json!({"resources": to_json(&e.result)});
                Ok(plain(
                    result,
                    output::resources(&e.result),
                    e.notifications,
                    e.elapsed,
                ))
            }
            McpOp::ResourcesRead { uri } => {
                let e = self.read_resource(uri, request).await?;
                let contents: Vec<Value> = e.result.iter().map(resource_json).collect();
                let text = output::contents(&e.result);
                let mut done = plain(
                    json!({"contents": contents}),
                    text,
                    e.notifications,
                    e.elapsed,
                );
                done.capture_body = resource_text(&e.result);
                Ok(done)
            }
            McpOp::PromptsList => {
                let e = self.list_prompts(request).await?;
                let result = json!({"prompts": to_json(&e.result)});
                Ok(plain(
                    result,
                    output::prompts(&e.result),
                    e.notifications,
                    e.elapsed,
                ))
            }
            McpOp::PromptsGet { name, arguments } => {
                let e = self.get_prompt(name, arguments, None, request).await?;
                let text = output::prompt(&e.result);
                Ok(plain(to_json(&e.result), text, e.notifications, e.elapsed))
            }
        }
    }

    async fn perform_call(
        &self,
        name: &str,
        arguments: &Value,
        options: &CallOptions,
    ) -> Result<OpResult, ClientError> {
        let exchange = self.call_tool(name, arguments.clone(), options).await?;
        let declared = self
            .cached_tools()
            .and_then(|tools| {
                tools
                    .iter()
                    .find(|t| t.name == name)
                    .map(|t| t.output_schema.is_some())
            })
            .unwrap_or(false);
        let tool = exchange.result;
        Ok(OpResult {
            result: tool_json(&tool),
            outcome: tool.label(),
            is_error: tool.is_error,
            output_violations: declared.then(|| tool.output_violations.clone()),
            capture_body: capture_body(&tool),
            text: output::tool_result(&tool),
            overridden: tool.overridden,
            notifications: exchange.notifications,
            elapsed: exchange.elapsed,
        })
    }
}

/// A result that is not a tool call's: captures read its JSON.
fn plain(
    result: Value,
    text: String,
    notifications: Vec<Notification>,
    elapsed: Duration,
) -> OpResult {
    OpResult {
        capture_body: result.to_string(),
        text,
        result,
        outcome: "ok",
        is_error: false,
        output_violations: None,
        overridden: Vec::new(),
        notifications,
        elapsed,
    }
}

/// A tool result in the MCP shape.
fn tool_json(tool: &ToolResult) -> Value {
    let mut result = json!({
        "content": serde_json::to_value(&tool.content).unwrap_or_default(),
        "isError": tool.is_error,
    });
    if let Some(structured) = &tool.structured {
        result["structuredContent"] = structured.clone();
    }
    result
}

fn to_json<T: serde::Serialize + ?Sized>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or_default()
}

/// The text of every text content, one per line.
fn resource_text(contents: &[ResourceContent]) -> String {
    contents
        .iter()
        .filter_map(|c| c.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n")
}

fn capture_body(tool: &ToolResult) -> String {
    match &tool.structured {
        Some(structured) => structured.to_string(),
        None => tool.text(),
    }
}

fn resource_json(content: &ResourceContent) -> Value {
    let mut value = json!({"uri": content.uri});
    if let Some(mime) = &content.mime_type {
        value["mimeType"] = json!(mime);
    }
    if let Some(text) = &content.text {
        value["text"] = json!(text);
    }
    if let Some(blob) = &content.blob {
        value["blob"] = json!(blob);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn operations_come_from_request_params() {
        let call = McpOp::from_params(
            McpOperation::ToolsCall,
            &params(json!({"name": "search", "arguments": {"q": "rust"}})),
        )
        .unwrap();
        assert_eq!(
            call,
            McpOp::ToolsCall {
                name: "search".into(),
                arguments: json!({"q": "rust"})
            }
        );
        assert_eq!(
            call.params(),
            params(json!({"name": "search", "arguments": {"q": "rust"}}))
        );
        let get = McpOp::from_params(
            McpOperation::PromptsGet,
            &params(json!({"name": "review", "arguments": {"code": "x", "n": 2}})),
        )
        .unwrap();
        let McpOp::PromptsGet { arguments, .. } = &get else {
            panic!("{get:?}");
        };
        assert_eq!(arguments["n"], "2");
        let err = McpOp::from_params(McpOperation::ResourcesRead, &Map::new()).unwrap_err();
        assert_eq!(err.to_string(), "mcp: resources/read requires params.uri");
        assert_eq!(
            McpOp::from_params(McpOperation::ToolsCall, &params(json!({"name": "t"})))
                .unwrap()
                .params()["arguments"],
            json!({})
        );
    }

    #[test]
    fn tool_results_take_the_mcp_shape() {
        let tool = ToolResult {
            tool: "count".into(),
            content: vec![super::super::tools::ContentItem::Text { text: "3".into() }],
            structured: Some(json!({"count": 3})),
            is_error: false,
            output_violations: Vec::new(),
            overridden: Vec::new(),
        };
        let result = tool_json(&tool);
        assert_eq!(result["content"][0], json!({"type": "text", "text": "3"}));
        assert_eq!(result["structuredContent"]["count"], 3);
        assert_eq!(capture_body(&tool), r#"{"count":3}"#);
        let text_only = ToolResult {
            structured: None,
            ..tool
        };
        assert_eq!(capture_body(&text_only), "3");
    }
}
