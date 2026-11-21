//! Workspace tools (spec "Workspace tools"): `list_requests`,
//! `get_request`, `list_environments` and `select_environment`.

use rustman_core::model::{KindRegistry, Request};
use rustman_core::workspace::RequestRef;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Call, output};
use crate::server::context::{AgentContext, ToolOutput};
use crate::server::error::{ErrorCode, ToolError};
use crate::server::limits::{paginate, snapshot};

/// No arguments.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListRequestsArgs {
    /// Only requests under this collection folder, such as `users`.
    pub collection: Option<String>,
    /// Only requests of this kind, such as `http`.
    pub kind: Option<String>,
    /// Only requests whose id, name or URL contains this text (any case).
    pub text: Option<String>,
    /// The `next_cursor` of the previous page.
    pub cursor: Option<String>,
    /// Items per page (default 50).
    pub limit: Option<usize>,
}

/// One saved request.
#[derive(Clone, Debug, Hash, Serialize, JsonSchema)]
pub struct RequestSummary {
    /// The path from the workspace root without `.rman.yaml`.
    pub id: String,
    /// The file, relative to the workspace root, with `/`.
    pub path: String,
    pub name: String,
    pub kind: String,
    /// HTTP requests only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// HTTP requests only, with its `{{variables}}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RequestList {
    pub items: Vec<RequestSummary>,
    /// Pass it as `cursor` for the next page; absent on the last one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// How many requests match, over every page.
    pub total: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetRequestArgs {
    /// The request's id (`users/get-user`) or file (`users/get-user.rman.yaml`).
    pub path: String,
}

/// One request file.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RequestFile {
    #[serde(flatten)]
    pub request: RequestSummary,
    /// The file's text, as saved: secrets are `{{name}}` references.
    pub text: String,
}

/// One environment.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EnvironmentInfo {
    pub name: String,
    /// Active in this session.
    pub active: bool,
    pub production: bool,
    /// Variable names (values are not listed).
    pub variables: Vec<String>,
    /// Secret names; their values are never returned.
    pub secrets: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EnvironmentList {
    pub environments: Vec<EnvironmentInfo>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectEnvironmentArgs {
    /// The environment to make active for this session.
    pub name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Selected {
    /// The session's active environment.
    pub environment: String,
}

fn summary(request: &RequestRef<'_>) -> RequestSummary {
    let entry = request.entry;
    let (method, url) = match &entry.request {
        Request::Http(http) => (
            Some(http.effective_method().to_owned()),
            Some(http.url.clone()),
        ),
        Request::Graphql(graphql) => (None, Some(graphql.endpoint.clone())),
        Request::Jsonrpc(jsonrpc) => (None, Some(jsonrpc.endpoint.clone())),
        Request::Grpc(grpc) => (None, Some(grpc.target.clone())),
        Request::Ws(ws) => (None, Some(ws.url.get().clone())),
        Request::Sse(sse) => (
            Some(sse.effective_method().to_owned()),
            Some(sse.url.get().clone()),
        ),
        Request::Messaging(m) => {
            let brokers = m.brokers.as_ref().map(|b| b.0.join(","));
            (None, brokers.or_else(|| m.broker.clone()))
        }
        Request::Mcp(_) | Request::Opaque(_) => (None, None),
    };
    RequestSummary {
        id: entry.id(),
        path: entry.path.to_string_lossy().replace('\\', "/"),
        name: entry.request.name().to_owned(),
        kind: entry.request.kind().to_owned(),
        method,
        url,
    }
}

/// Whether `request` matches every filter of `args`.
fn matches(request: &RequestSummary, args: &ListRequestsArgs) -> bool {
    let in_collection = args.collection.as_deref().is_none_or(|c| {
        let folder = c.trim_matches('/');
        folder.is_empty() || request.id.starts_with(&format!("{folder}/"))
    });
    let of_kind = args.kind.as_deref().is_none_or(|k| request.kind == k);
    let text = args.text.as_deref().map(str::to_lowercase);
    let has_text = text.is_none_or(|t| {
        [Some(&request.id), Some(&request.name), request.url.as_ref()]
            .into_iter()
            .flatten()
            .any(|field| field.to_lowercase().contains(&t))
    });
    in_collection && of_kind && has_text
}

pub fn list_requests(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: ListRequestsArgs = call.args()?;
    let found: Vec<RequestSummary> = call
        .ctx
        .workspace()
        .requests()
        .iter()
        .map(summary)
        .filter(|r| matches(r, &args))
        .collect();
    let total = found.len();
    let page = paginate(
        found.clone(),
        snapshot(&found),
        args.cursor.as_deref(),
        args.limit,
    )?;
    let shown = page.items.len();
    let list = RequestList {
        items: page.items,
        next_cursor: page.next_cursor,
        total,
    };
    output(&list, format!("{shown} of {total} requests"))
}

/// The saved request `path` names: its id or its file.
pub fn find<'w>(ctx: &'w AgentContext, path: &str) -> Result<RequestRef<'w>, ToolError> {
    let wanted = path.trim_start_matches("./").replace('\\', "/");
    ctx.workspace()
        .requests()
        .into_iter()
        .find(|r| {
            r.entry.id() == wanted || r.entry.path.to_string_lossy().replace('\\', "/") == wanted
        })
        .ok_or_else(|| ToolError::new(ErrorCode::NotFound, format!("no saved request '{path}'")))
}

pub fn get_request(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: GetRequestArgs = call.args()?;
    let request = find(&call.ctx, &args.path)?;
    let text = request_text(&call.ctx, &request)?;
    let result = RequestFile {
        request: summary(&request),
        text,
    };
    let line = format!("{} ({})", result.request.id, result.request.kind);
    output(&result, line)
}

/// The text of a saved request file, with the literal credentials it
/// holds (sensitive header values, its auth block) and every session secret
/// redacted. The session knows the literals of the workspace as loaded; the
/// file is parsed again as read, so an edit since cannot slip a credential
/// through, and a file that no longer parses is not returned.
pub fn request_text(ctx: &AgentContext, request: &RequestRef) -> Result<String, ToolError> {
    let path = &request.entry.path;
    let text = std::fs::read_to_string(request.root.join(path))
        .map_err(|e| ToolError::internal(format!("cannot read {}: {e}", path.display())))?;
    let current = KindRegistry::default()
        .with_opaque_fallback()
        .load(path, &text)
        .map_err(|e| ToolError::internal(format!("cannot read {}: {e}", path.display())))?;
    Ok(ctx.redactor().with_request_literals(&current).text(&text))
}

pub fn list_environments(call: &Call) -> Result<ToolOutput, ToolError> {
    let _: NoArgs = call.args()?;
    let active = call.ctx.environment().map(|e| e.name);
    let environments: Vec<EnvironmentInfo> = call
        .ctx
        .workspace()
        .environments
        .iter()
        .map(|e| EnvironmentInfo {
            name: e.name.clone(),
            active: active.as_deref() == Some(e.name.as_str()),
            production: e.production,
            variables: e.variables.iter().map(|(n, _)| n.to_owned()).collect(),
            secrets: e.secrets.clone(),
        })
        .collect();
    let line = format!(
        "{} environments, active: {}",
        environments.len(),
        active.as_deref().unwrap_or("none")
    );
    output(&EnvironmentList { environments }, line)
}

pub fn select_environment(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: SelectEnvironmentArgs = call.args()?;
    call.ctx.select_environment(&args.name)?;
    let line = format!("environment '{}' is active for this session", args.name);
    output(
        &Selected {
            environment: args.name,
        },
        line,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str, url: &str) -> RequestSummary {
        RequestSummary {
            id: id.into(),
            path: format!("{id}.rman.yaml"),
            name: id.rsplit('/').next().unwrap_or(id).into(),
            kind: "http".into(),
            method: Some("GET".into()),
            url: Some(url.into()),
        }
    }

    #[test]
    fn filters_combine() {
        let users = request("users/get-user", "{{base_url}}/users/1");
        let orders = request("orders/list", "{{base_url}}/orders");
        let by_collection = ListRequestsArgs {
            collection: Some("users/".into()),
            ..ListRequestsArgs::default()
        };
        assert!(matches(&users, &by_collection) && !matches(&orders, &by_collection));
        let by_text = ListRequestsArgs {
            text: Some("ORDERS".into()),
            kind: Some("http".into()),
            ..ListRequestsArgs::default()
        };
        assert!(!matches(&users, &by_text) && matches(&orders, &by_text));
        let other_kind = ListRequestsArgs {
            kind: Some("graphql".into()),
            ..ListRequestsArgs::default()
        };
        assert!(!matches(&users, &other_kind));
    }
}
