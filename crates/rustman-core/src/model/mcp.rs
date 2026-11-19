//! The `kind: mcp` request file (mcp-client "MCP request files"): a server
//! of the workspace, an operation and its params.
//!
//! Execution belongs to the `rustman-mcp` crate; this module only holds the
//! file model and its load-time checks.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::assertion::Assertion;
use super::capture::Capture;
use super::common::{Checked, Variables};
use super::kind::REQUEST_VERSION;
use crate::duration::parse_duration;
use crate::error::LoadError;
use crate::format::from_yaml;

/// What an mcp request does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum McpOperation {
    #[serde(rename = "tools/list")]
    ToolsList,
    #[serde(rename = "tools/call")]
    ToolsCall,
    #[serde(rename = "resources/list")]
    ResourcesList,
    #[serde(rename = "resources/read")]
    ResourcesRead,
    #[serde(rename = "prompts/list")]
    PromptsList,
    #[serde(rename = "prompts/get")]
    PromptsGet,
}

impl McpOperation {
    /// The JSON-RPC method, as written in request files.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolsList => "tools/list",
            Self::ToolsCall => "tools/call",
            Self::ResourcesList => "resources/list",
            Self::ResourcesRead => "resources/read",
            Self::PromptsList => "prompts/list",
            Self::PromptsGet => "prompts/get",
        }
    }

    /// The string param the operation needs: `name` or `uri`.
    pub fn required_param(self) -> Option<&'static str> {
        match self {
            Self::ToolsCall | Self::PromptsGet => Some("name"),
            Self::ResourcesRead => Some("uri"),
            Self::ToolsList | Self::ResourcesList | Self::PromptsList => None,
        }
    }
}

impl fmt::Display for McpOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A `kind: mcp` request file. Saving writes `source` back unchanged; a
/// request built in the TUI (empty `source`) is written field by field.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpRequest {
    pub version: u64,
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    /// The name of a server the workspace defines.
    pub server: String,
    pub operation: McpOperation,
    /// `name` and `arguments` for tools and prompts, `uri` for resources.
    /// String values are interpolated.
    pub params: Option<Map<String, Value>>,
    /// Overrides the server's request timeout (`500ms`, `30s`, `2m`).
    #[serde(default, deserialize_with = "duration")]
    pub timeout: Option<Duration>,
    /// `false` sends tool arguments that break `inputSchema` anyway.
    pub validate: Option<bool>,
    /// The request's layer of Rustman variables.
    pub variables: Option<Variables>,
    pub assertions: Option<Vec<Assertion>>,
    /// Values taken from `structuredContent` (or the text content).
    pub captures: Option<Vec<Checked<Capture>>>,
    /// The file text, written back unchanged on save.
    #[serde(skip)]
    pub source: String,
}

fn duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Duration>, D::Error> {
    let text = String::deserialize(deserializer)?;
    parse_duration(&text).map(Some).map_err(de::Error::custom)
}

impl McpRequest {
    /// A new request named `name` (no file yet), as the TUI saves a call.
    pub fn new(
        name: &str,
        server: &str,
        operation: McpOperation,
        params: Map<String, Value>,
    ) -> Self {
        Self {
            version: REQUEST_VERSION,
            kind: "mcp".into(),
            name: name.to_owned(),
            description: None,
            tags: None,
            server: server.to_owned(),
            operation,
            params: (!params.is_empty()).then_some(params),
            timeout: None,
            validate: None,
            variables: None,
            assertions: None,
            captures: None,
            source: String::new(),
        }
    }

    /// Parse and check the `kind: mcp` file `file` whose text is `text`.
    pub fn parse(file: &Path, text: &str) -> Result<Self, LoadError> {
        let mut request: Self = from_yaml(file, text)?;
        request.check().map_err(|m| LoadError::new(file, None, m))?;
        request.source = text.to_owned();
        Ok(request)
    }

    /// The load-time checks: a server, and the param the operation needs.
    pub fn check(&self) -> Result<(), String> {
        if self.server.trim().is_empty() {
            return Err("mcp: server must not be empty".into());
        }
        let params = self.params.as_ref();
        if let Some(name) = self.operation.required_param() {
            let given = params.and_then(|p| p.get(name)).and_then(Value::as_str);
            if given.is_none_or(|v| v.trim().is_empty()) {
                return Err(format!(
                    "mcp: {} requires params.{name} (a string)",
                    self.operation
                ));
            }
        }
        let arguments = params.and_then(|p| p.get("arguments"));
        if arguments.is_some_and(|a| !a.is_object()) {
            return Err("mcp: params.arguments must be a map".into());
        }
        Ok(())
    }

    /// The params, empty when the file has none.
    pub fn params(&self) -> Map<String, Value> {
        self.params.clone().unwrap_or_default()
    }

    /// Whether tool arguments are validated before the call (default true).
    pub fn effective_validate(&self) -> bool {
        self.validate.unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<McpRequest, LoadError> {
        McpRequest::parse(Path::new("m.rman.yaml"), text)
    }

    #[test]
    fn a_tool_call_loads() {
        let text = "version: 1\nkind: mcp\nname: Search\nserver: files\noperation: tools/call\nparams:\n  name: search\n  arguments: {q: rust}\ntimeout: 5s\n";
        let request = parse(text).unwrap();
        assert_eq!(request.operation, McpOperation::ToolsCall);
        assert_eq!(request.params()["arguments"]["q"], "rust");
        assert_eq!(request.timeout, Some(Duration::from_secs(5)));
        assert!(request.effective_validate());
    }

    #[test]
    fn operations_need_their_params() {
        let base = "version: 1\nkind: mcp\nname: x\nserver: files\n";
        let err = parse(&format!("{base}operation: tools/call\n")).unwrap_err();
        assert_eq!(
            err.message,
            "mcp: tools/call requires params.name (a string)"
        );
        let err = parse(&format!("{base}operation: resources/read\nparams: {{}}\n")).unwrap_err();
        assert_eq!(
            err.message,
            "mcp: resources/read requires params.uri (a string)"
        );
        let err = parse(&format!(
            "{base}operation: tools/call\nparams: {{name: a, arguments: [1]}}\n"
        ))
        .unwrap_err();
        assert_eq!(err.message, "mcp: params.arguments must be a map");
        assert!(parse(&format!("{base}operation: tools/list\n")).is_ok());
        let err = parse(&format!("{base}operation: tools/run\n")).unwrap_err();
        assert!(err.message.contains("tools/run"), "{}", err.message);
    }

    #[test]
    fn an_empty_server_is_rejected() {
        let text = "version: 1\nkind: mcp\nname: x\nserver: ' '\noperation: tools/list\n";
        assert_eq!(
            parse(text).unwrap_err().message,
            "mcp: server must not be empty"
        );
    }
}
