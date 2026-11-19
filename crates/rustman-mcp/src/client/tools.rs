//! Tools (mcp-client spec "Tool listing", "Client-side argument
//! validation", "Tool call results", "Output schema validation"): the
//! paginated list, kept current on `notifications/tools/list_changed`, and
//! calls validated against `inputSchema` before they are sent and against
//! `outputSchema` once answered.

use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, CallToolResult, ClientRequest, ListToolsRequest,
    ServerResult, Tool,
};
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::sync::watch;

use super::error::ClientError;
use super::info::Operation;
use super::notify::redact_value;
use super::request::{Exchange, RequestOptions, Sender};
use super::schema::{self, Violation};
use super::session::McpSession;

/// The tools last listed, and a version bumped on every change.
pub(crate) struct ToolCatalog {
    tools: Mutex<Option<Arc<Vec<Tool>>>>,
    version: watch::Sender<u64>,
}

impl Default for ToolCatalog {
    fn default() -> Self {
        Self {
            tools: Mutex::default(),
            version: watch::Sender::new(0),
        }
    }
}

impl ToolCatalog {
    pub fn get(&self) -> Option<Arc<Vec<Tool>>> {
        self.tools
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set(&self, tools: Vec<Tool>) -> Arc<Vec<Tool>> {
        let tools = Arc::new(tools);
        *self.tools.lock().unwrap_or_else(PoisonError::into_inner) = Some(tools.clone());
        self.version.send_modify(|v| *v += 1);
        tools
    }

    pub fn changes(&self) -> watch::Receiver<u64> {
        self.version.subscribe()
    }
}

/// Every tool, following `nextCursor`.
pub(crate) async fn list(
    sender: &Sender,
    catalog: &ToolCatalog,
    options: &RequestOptions,
) -> Result<Exchange<Arc<Vec<Tool>>>, ClientError> {
    let exchange = sender
        .paginate(
            "tools/list",
            options,
            |params| ClientRequest::ListToolsRequest(ListToolsRequest::with_param(params)),
            |result| match result {
                ServerResult::ListToolsResult(page) => Some((page.tools, page.next_cursor)),
                _ => None,
            },
        )
        .await?;
    Ok(exchange.map(|tools| catalog.set(tools)))
}

/// One content item of a tool result, as shown.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    Text {
        text: String,
    },
    /// An image: its type and size, for a preview.
    Image {
        mime_type: String,
        bytes: usize,
    },
    Audio {
        mime_type: String,
        bytes: usize,
    },
    ResourceLink {
        uri: String,
        name: Option<String>,
        mime_type: Option<String>,
    },
    /// An embedded resource: its text, or the size of its blob.
    Resource {
        uri: String,
        mime_type: Option<String>,
        text: Option<String>,
        blob_bytes: Option<usize>,
    },
    Other {
        value: Value,
    },
}

impl ContentItem {
    pub(crate) fn from_value(value: &Value) -> Self {
        let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
        let size = |v: &Value| text(v, "data").map_or(0, |d| decoded_len(&d));
        match value.get("type").and_then(Value::as_str) {
            Some("text") => Self::Text {
                text: text(value, "text").unwrap_or_default(),
            },
            Some("image") => Self::Image {
                mime_type: text(value, "mimeType").unwrap_or_default(),
                bytes: size(value),
            },
            Some("audio") => Self::Audio {
                mime_type: text(value, "mimeType").unwrap_or_default(),
                bytes: size(value),
            },
            Some("resource_link") => Self::ResourceLink {
                uri: text(value, "uri").unwrap_or_default(),
                name: text(value, "name"),
                mime_type: text(value, "mimeType"),
            },
            Some("resource") => {
                let resource = value.get("resource").cloned().unwrap_or_default();
                Self::Resource {
                    uri: text(&resource, "uri").unwrap_or_default(),
                    mime_type: text(&resource, "mimeType"),
                    text: text(&resource, "text"),
                    blob_bytes: text(&resource, "blob").map(|b| decoded_len(&b)),
                }
            }
            _ => Self::Other {
                value: value.clone(),
            },
        }
    }

    /// One line for a result view.
    pub fn summary(&self) -> String {
        match self {
            Self::Text { text } => text.clone(),
            Self::Image { mime_type, bytes } => format!("[image {mime_type}, {bytes} bytes]"),
            Self::Audio { mime_type, bytes } => format!("[audio {mime_type}, {bytes} bytes]"),
            Self::ResourceLink { uri, .. } => format!("[resource link {uri}]"),
            Self::Resource { uri, text, .. } => match text {
                Some(text) => format!("[resource {uri}] {text}"),
                None => format!("[resource {uri}]"),
            },
            Self::Other { value } => value.to_string(),
        }
    }
}

fn decoded_len(data: &str) -> usize {
    STANDARD
        .decode(data)
        .map_or(data.len() * 3 / 4, |bytes| bytes.len())
}

/// The answer to `tools/call`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolResult {
    pub tool: String,
    pub content: Vec<ContentItem>,
    /// `structuredContent`.
    pub structured: Option<Value>,
    /// `isError: true`: the tool failed (not the protocol).
    pub is_error: bool,
    /// Where `structuredContent` breaks the tool's `outputSchema`.
    pub output_violations: Vec<Violation>,
    /// Input violations the user chose to send anyway (recorded in history).
    pub overridden: Vec<Violation>,
}

impl ToolResult {
    /// `tool error` for `isError` results, else `ok`.
    pub fn label(&self) -> &'static str {
        if self.is_error { "tool error" } else { "ok" }
    }

    /// The text content items, one per line.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| match c {
                ContentItem::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// How a tool is called.
#[derive(Clone, Debug)]
pub struct CallOptions {
    pub request: RequestOptions,
    /// `false` sends arguments that break `inputSchema` anyway
    /// (`--no-validate`, or the TUI's explicit confirm).
    pub validate: bool,
}

impl Default for CallOptions {
    fn default() -> Self {
        Self {
            request: RequestOptions::default(),
            validate: true,
        }
    }
}

/// Violations of `tool`'s `inputSchema` by `arguments`.
pub fn validate_arguments(tool: &Tool, arguments: &Value) -> Vec<Violation> {
    let schema = Value::Object((*tool.input_schema).clone());
    schema::validate(&schema, arguments).unwrap_or_else(|e| vec![Violation::new("/", e)])
}

/// Violations of `tool`'s `outputSchema` by `structured`; none without one.
pub fn validate_output(tool: &Tool, structured: Option<&Value>) -> Vec<Violation> {
    let Some(output) = &tool.output_schema else {
        return Vec::new();
    };
    let Some(structured) = structured else {
        return vec![Violation::new("/", "structuredContent missing")];
    };
    let schema = Value::Object((**output).clone());
    schema::validate(&schema, structured).unwrap_or_else(|e| vec![Violation::new("/", e)])
}

impl McpSession {
    /// `tools/list`, every page.
    pub async fn list_tools(
        &self,
        options: &RequestOptions,
    ) -> Result<Exchange<Arc<Vec<Tool>>>, ClientError> {
        self.require(Operation::ListTools)?;
        list(&self.sender(), &self.state().tools, options).await
    }

    /// The tools last listed (refetched on `list_changed`).
    pub fn cached_tools(&self) -> Option<Arc<Vec<Tool>>> {
        self.state().tools.get()
    }

    /// Changes whenever the tool list is refetched.
    pub fn tool_changes(&self) -> watch::Receiver<u64> {
        self.state().tools.changes()
    }

    /// The tool named `name`, listing the tools first if needed.
    pub async fn tool(&self, name: &str) -> Result<Option<Tool>, ClientError> {
        self.find_tool(name, &RequestOptions::default()).await
    }

    /// [`Self::tool`], listing the tools (when not cached yet) with
    /// `options`, so the call's timeout and cancellation cover the lookup.
    async fn find_tool(
        &self,
        name: &str,
        options: &RequestOptions,
    ) -> Result<Option<Tool>, ClientError> {
        let tools = match self.cached_tools() {
            Some(tools) => tools,
            None => self.list_tools(options).await?.result,
        };
        Ok(tools.iter().find(|t| t.name == name).cloned())
    }

    /// `tools/call` with `arguments` (a JSON object), validated first.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        options: &CallOptions,
    ) -> Result<Exchange<ToolResult>, ClientError> {
        self.require(Operation::CallTool)?;
        let lookup = RequestOptions {
            progress: None,
            ..options.request.clone()
        };
        let tool = self.find_tool(name, &lookup).await?;
        let overridden = match &tool {
            Some(tool) => self.check_arguments(tool, &arguments, options.validate)?,
            None if options.validate => return Err(ClientError::UnknownTool(name.to_owned())),
            None => Vec::new(),
        };
        let params = CallToolRequestParams::new(name.to_owned()).with_arguments(object(arguments)?);
        let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
        let sender = self.sender();
        let exchange = sender.send(request, &options.request).await?;
        let ServerResult::CallToolResult(result) = exchange.result.clone() else {
            return Err(sender.unexpected("tools/call"));
        };
        let result = self.tool_result(name, result, tool.as_ref(), overridden);
        Ok(exchange.map(|_| result))
    }

    fn check_arguments(
        &self,
        tool: &Tool,
        arguments: &Value,
        validate: bool,
    ) -> Result<Vec<Violation>, ClientError> {
        let violations = validate_arguments(tool, arguments);
        if validate && !violations.is_empty() {
            return Err(ClientError::InvalidArguments(violations));
        }
        Ok(violations)
    }

    fn tool_result(
        &self,
        name: &str,
        result: CallToolResult,
        tool: Option<&Tool>,
        overridden: Vec<Violation>,
    ) -> ToolResult {
        let redactor = self.redactor();
        let structured = result
            .structured_content
            .map(|v| redact_value(&redactor, v));
        let output_violations = tool
            .map(|t| validate_output(t, structured.as_ref()))
            .unwrap_or_default();
        let content = result
            .content
            .iter()
            .filter_map(|c| serde_json::to_value(c).ok())
            .map(|v| ContentItem::from_value(&redact_value(&redactor, v)))
            .collect();
        ToolResult {
            tool: name.to_owned(),
            content,
            structured,
            is_error: result.is_error.unwrap_or(false),
            output_violations,
            overridden,
        }
    }
}

fn object(arguments: Value) -> Result<Map<String, Value>, ClientError> {
    match arguments {
        Value::Object(map) => Ok(map),
        Value::Null => Ok(Map::new()),
        other => Err(ClientError::Invalid(format!(
            "tool arguments must be a JSON object, not {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool(input: Value, output: Option<Value>) -> Tool {
        let mut value = json!({"name": "t", "inputSchema": input});
        if let Some(output) = output {
            value["outputSchema"] = output;
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn content_items_by_type() {
        let items = [
            json!({"type": "text", "text": "hi"}),
            json!({"type": "image", "mimeType": "image/png", "data": "AAAA"}),
            json!({"type": "audio", "mimeType": "audio/wav", "data": "AAAAAAAA"}),
            json!({"type": "resource_link", "uri": "file:///a", "name": "a"}),
            json!({"type": "resource", "resource": {"uri": "file:///b", "text": "body"}}),
        ];
        let shown: Vec<String> = items
            .iter()
            .map(|v| ContentItem::from_value(v).summary())
            .collect();
        assert_eq!(
            shown,
            [
                "hi",
                "[image image/png, 3 bytes]",
                "[audio audio/wav, 6 bytes]",
                "[resource link file:///a]",
                "[resource file:///b] body"
            ]
        );
    }

    #[test]
    fn output_must_match_the_output_schema() {
        let t = tool(
            json!({"type": "object"}),
            Some(
                json!({"type": "object", "properties": {"count": {"type": "integer"}}, "required": ["count"]}),
            ),
        );
        let bad = validate_output(&t, Some(&json!({"count": "3"})));
        assert_eq!(bad[0].to_string(), "/count: expected integer");
        assert!(validate_output(&t, Some(&json!({"count": 3}))).is_empty());
        assert_eq!(
            validate_output(&t, None)[0].to_string(),
            "/: structuredContent missing"
        );
        let free = tool(json!({"type": "object"}), None);
        assert!(validate_output(&free, None).is_empty());
    }

    #[test]
    fn arguments_must_match_the_input_schema() {
        let t = tool(
            json!({"type": "object", "required": ["path"], "properties": {"path": {"type": "string"}}}),
            None,
        );
        let violations = validate_arguments(&t, &json!({}));
        assert_eq!(
            ClientError::InvalidArguments(violations).to_string(),
            "mcp: invalid arguments: /path: required property missing"
        );
    }

    #[test]
    fn tool_errors_and_protocol_errors_are_labeled_apart() {
        let result = ToolResult {
            tool: "t".into(),
            content: Vec::new(),
            structured: None,
            is_error: true,
            output_violations: Vec::new(),
            overridden: Vec::new(),
        };
        assert_eq!(result.label(), "tool error");
        let protocol = ClientError::Protocol {
            code: -32602,
            message: "bad".into(),
        };
        assert_eq!(protocol.label(), "protocol error -32602");
        assert_eq!(protocol.to_string(), "mcp: protocol error -32602: bad");
    }
}
