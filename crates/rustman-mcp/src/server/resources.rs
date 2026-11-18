//! Resources (spec "Resources"): request files as `rustman://requests/<path>`,
//! reports as `rustman://reports/<id>`, cached schemas as
//! `rustman://schemas/<name>` and truncated bodies as
//! `rustman://bodies/<id>`. Every content is redacted, and a path that could
//! leave the workspace is refused before anything is read.

use std::path::{Component, Path};

use base64::Engine as _;
use rmcp::ErrorData;
use rmcp::model::{Resource, ResourceContents, ResourceTemplate};
use rustman_explore::explore::session::{Session, explorations_dir};
use rustman_explore::store::SNAPSHOT_DIR;

use super::context::AgentContext;
use super::jobs::{JobKind, JobState};
use super::limits::BODY_URI_PREFIX;
use super::tools::reports::find_report;
use super::tools::workspace::request_text;

pub const REQUESTS: &str = "rustman://requests/";
pub const REPORTS: &str = "rustman://reports/";
pub const SCHEMAS: &str = "rustman://schemas/";

/// The name of the report JSON Schema under `rustman://schemas/`.
pub const REPORT_SCHEMA: &str = "report";

/// The URI templates of every kind of resource.
pub fn templates() -> Vec<ResourceTemplate> {
    vec![
        ResourceTemplate::new(format!("{REQUESTS}{{path}}"), "request")
            .with_description("A saved request file, by its path in the workspace")
            .with_mime_type("application/yaml"),
        ResourceTemplate::new(format!("{REPORTS}{{id}}"), "report")
            .with_description("A rustman.report/v1 report: an exploration id or a run's job id")
            .with_mime_type("application/json"),
        ResourceTemplate::new(format!("{SCHEMAS}{{name}}"), "schema")
            .with_description("The report JSON Schema (`report`) or an imported OpenAPI snapshot")
            .with_mime_type("application/json"),
        ResourceTemplate::new(format!("{BODY_URI_PREFIX}{{id}}"), "body")
            .with_description("The whole redacted body of a truncated response (body_ref)"),
    ]
}

/// Every resource of the session, in a stable order.
pub fn list(ctx: &AgentContext) -> Vec<Resource> {
    let mut resources: Vec<Resource> = ctx
        .workspace()
        .requests()
        .iter()
        .map(|r| {
            let path = r.entry.path.to_string_lossy().replace('\\', "/");
            Resource::new(format!("{REQUESTS}{path}"), r.entry.id())
                .with_mime_type("application/yaml")
        })
        .collect();
    resources.extend(
        report_ids(ctx).into_iter().map(|id| {
            Resource::new(format!("{REPORTS}{id}"), id).with_mime_type("application/json")
        }),
    );
    resources.extend(schema_names(&ctx.workspace().root).into_iter().map(|name| {
        Resource::new(format!("{SCHEMAS}{name}"), name).with_mime_type("application/json")
    }));
    resources
}

/// Explorations with a report, then finished runs of this session.
fn report_ids(ctx: &AgentContext) -> Vec<String> {
    let root = &ctx.workspace().root;
    let mut ids: Vec<String> = std::fs::read_dir(explorations_dir(root))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| Session::open(root, &e.file_name().to_string_lossy()).ok())
        .filter(|s| s.report_path().is_file())
        .map(|s| s.state.id)
        .collect();
    ids.sort();
    ids.extend(
        ctx.jobs()
            .all()
            .into_iter()
            .filter(|j| j.kind == JobKind::Run && j.state() != JobState::Running)
            .map(|j| j.id.clone()),
    );
    ids
}

/// `report`, then the OpenAPI snapshots in `.rustman/openapi/`.
fn schema_names(root: &Path) -> Vec<String> {
    let dir = snapshot_dir(root);
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".json").map(str::to_owned)
        })
        .collect();
    names.sort();
    names.insert(0, REPORT_SCHEMA.to_owned());
    names
}

fn snapshot_dir(root: &Path) -> std::path::PathBuf {
    SNAPSHOT_DIR
        .split('/')
        .fold(root.to_path_buf(), |p, s| p.join(s))
}

/// `rest` when it is a plain relative path: no `..`, no root, no drive,
/// no backslash.
fn safe(rest: &str) -> Result<&str, ErrorData> {
    let path = Path::new(rest);
    let plain = !rest.is_empty()
        && !rest.contains('\\')
        && path.components().all(|c| matches!(c, Component::Normal(_)));
    if plain {
        Ok(rest)
    } else {
        Err(ErrorData::invalid_params(
            format!("'{rest}' is not a path inside the workspace"),
            None,
        ))
    }
}

fn not_found(uri: &str) -> ErrorData {
    ErrorData::resource_not_found(format!("no resource '{uri}'"), None)
}

/// Read `uri`, redacted.
pub fn read(ctx: &AgentContext, uri: &str) -> Result<ResourceContents, ErrorData> {
    if let Some(rest) = uri.strip_prefix(REQUESTS) {
        return read_request(ctx, safe(rest)?, uri);
    }
    if let Some(rest) = uri.strip_prefix(REPORTS) {
        return read_report(ctx, safe(rest)?, uri);
    }
    if let Some(rest) = uri.strip_prefix(SCHEMAS) {
        return read_schema(ctx, safe(rest)?, uri);
    }
    if let Some(rest) = uri.strip_prefix(BODY_URI_PREFIX) {
        return read_body(ctx, safe(rest)?, uri);
    }
    Err(not_found(uri))
}

fn text(ctx: &AgentContext, text: &str, uri: &str, mime: &str) -> ResourceContents {
    ResourceContents::TextResourceContents {
        uri: uri.to_owned(),
        mime_type: Some(mime.to_owned()),
        text: ctx.redactor().text(text),
        meta: None,
    }
}

fn json(
    ctx: &AgentContext,
    value: &serde_json::Value,
    uri: &str,
) -> Result<ResourceContents, ErrorData> {
    let redacted = ctx
        .redactor()
        .json(value)
        .map_err(|e| ErrorData::internal_error(e.message, None))?;
    let pretty = serde_json::to_string_pretty(&redacted)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
    Ok(ResourceContents::TextResourceContents {
        uri: uri.to_owned(),
        mime_type: Some("application/json".into()),
        text: pretty,
        meta: None,
    })
}

/// A request file of the workspace: only files the workspace loaded as
/// requests are readable.
fn read_request(ctx: &AgentContext, path: &str, uri: &str) -> Result<ResourceContents, ErrorData> {
    let ws = ctx.workspace();
    let entry = ws
        .requests()
        .into_iter()
        .find(|r| r.entry.path.to_string_lossy().replace('\\', "/") == path)
        .ok_or_else(|| not_found(uri))?;
    let content =
        request_text(ctx, &entry).map_err(|e| ErrorData::internal_error(e.message, None))?;
    Ok(text(ctx, &content, uri, "application/yaml"))
}

fn read_report(ctx: &AgentContext, id: &str, uri: &str) -> Result<ResourceContents, ErrorData> {
    let (_, report) = find_report(ctx, Some(id)).map_err(|_| not_found(uri))?;
    let value: serde_json::Value = serde_json::from_str(&report.to_json())
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
    json(ctx, &value, uri)
}

fn read_schema(ctx: &AgentContext, name: &str, uri: &str) -> Result<ResourceContents, ErrorData> {
    if name == REPORT_SCHEMA {
        let schema = rustman_report::report_schema();
        let value: serde_json::Value = serde_json::from_str(&schema)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        return json(ctx, &value, uri);
    }
    if name.contains('/') {
        return Err(not_found(uri));
    }
    let file = snapshot_dir(&ctx.workspace().root).join(format!("{name}.json"));
    let content = std::fs::read_to_string(file).map_err(|_| not_found(uri))?;
    Ok(text(ctx, &content, uri, "application/json"))
}

fn read_body(ctx: &AgentContext, id: &str, uri: &str) -> Result<ResourceContents, ErrorData> {
    let body = ctx.bodies().get(id).ok_or_else(|| not_found(uri))?;
    Ok(match std::str::from_utf8(&body) {
        Ok(content) => text(ctx, content, uri, "text/plain"),
        Err(_) => blob(ctx, &body, uri),
    })
}

/// A binary body, redacted byte-wise with the session's secrets (including
/// those learned after it was stored) before it is encoded.
fn blob(ctx: &AgentContext, body: &[u8], uri: &str) -> ResourceContents {
    let redactor = ctx.redactor();
    let body = redactor.redactor().bytes(body);
    ResourceContents::BlobResourceContents {
        uri: uri.to_owned(),
        mime_type: Some("application/octet-stream".into()),
        blob: base64::engine::general_purpose::STANDARD.encode(&body),
        meta: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_relative_paths_are_safe() {
        assert!(safe("users/get-user.rman.yaml").is_ok());
        for bad in [
            "../../etc/passwd",
            "a/../../b",
            "/etc/passwd",
            "a\\..\\b",
            "",
            "./a",
        ] {
            assert!(safe(bad).is_err(), "{bad}");
        }
    }
}
