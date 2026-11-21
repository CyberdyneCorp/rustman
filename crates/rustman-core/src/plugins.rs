//! Plugins as the core sees them (add-plugin-system): the extension points
//! a loaded plugin set serves, and the trust prompt a front-end shows
//! before any plugin code runs. The plugin host (`rustman-plugin`)
//! implements both traits; the core and the front-ends only call them, so
//! nothing here depends on the WebAssembly runtime.

use std::path::Path;

use crate::Redactor;

/// One plugin of a set waiting for trust, as the prompt shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginSummary {
    pub name: String,
    pub version: String,
    /// The `source` of its `[plugins.<name>]` table.
    pub source: String,
    /// Each grant in plain language, such as `send HTTP requests to
    /// api.example.com`; empty when it holds none.
    pub grants: Vec<String>,
    /// Its per-call limits, such as `execution time 100 ms per call,
    /// memory 64 MiB`.
    pub limits: String,
}

/// A plugin set that this machine has not trusted yet (plugins spec,
/// "Workspace plugin trust").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginTrustPrompt {
    pub plugins: Vec<PluginSummary>,
    /// Trusted before, with other plugins, hashes or grants.
    pub changed: bool,
    /// The hash of the set shown: approving records exactly this set.
    pub fingerprint: String,
}

/// Admits the network requests of plugins in agent sessions (`rustman mcp
/// serve`): on top of a plugin's `net:` grants, each request must reach
/// an origin the agent may reach, and is audited. The agent interface
/// (`rustman-mcp`) implements it; the plugin host calls it before every
/// request a plugin sends.
pub trait PluginNetGuard: Send + Sync {
    /// Admit `method url` that `plugin` is about to send, or refuse it with
    /// the reason. `shown` is the URL redacted, for the audit log. A
    /// request that cannot be audited is refused.
    fn admit(&self, plugin: &str, method: &str, url: &str, shown: &str) -> Result<(), String>;
}

/// Checks and records the trust of a workspace's plugin set. Neither call
/// runs plugin code.
pub trait PluginGate: Send + Sync {
    /// `None` when the workspace declares no plugin or its set is trusted.
    fn check(&self, root: &Path) -> Result<Option<PluginTrustPrompt>, String>;
    /// Trust the set `prompt` showed. Fails when the set changed since.
    fn approve(&self, root: &Path, prompt: &PluginTrustPrompt) -> Result<(), String>;
    /// The request kinds (with their declarative panes) the plugins of a
    /// trusted set register; empty when the set is not trusted. This loads
    /// the set, so it runs plugin code: only call it once trusted.
    fn kinds(&self, root: &Path) -> Result<Vec<PluginKind>, String> {
        let _ = root;
        Ok(Vec::new())
    }
    /// The extension points of a trusted set, opened for a send in
    /// `environment` with the set secrets of `store`; `None` when the
    /// workspace declares no plugin or its set is not trusted. This loads
    /// the set, so it runs plugin code: it never asks for or records trust.
    fn extensions(
        &self,
        root: &Path,
        environment: Option<&crate::Environment>,
        store: &crate::SecretStore,
    ) -> Result<Option<std::sync::Arc<dyn Extensions>>, String> {
        let _ = (root, environment, store);
        Ok(None)
    }
}

/// A name and a value: a header or a query parameter.
pub type Pair = (String, String);

/// The fully resolved request an auth scheme signs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<Pair>,
    pub body: Vec<u8>,
    /// The other fields of the `auth` block, as strings.
    pub params: Vec<Pair>,
}

/// What signing adds to a request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignOutput {
    /// Headers to add, replacing those with the same name.
    pub set_headers: Vec<Pair>,
    pub add_query: Vec<Pair>,
    /// A new URL; only its path and query may differ.
    pub url: Option<String>,
}

/// The redacted exchange an assertion function checks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginExchange {
    pub method: String,
    pub url: String,
    pub request_headers: Vec<Pair>,
    pub request_body: Vec<u8>,
    pub status: u16,
    pub response_headers: Vec<Pair>,
    pub response_body: Vec<u8>,
    pub duration_ms: u64,
}

/// How an assertion function ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginVerdict {
    Pass,
    Fail(String),
    Error(String),
}

/// A request or response body and its headers, as transformers see it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginMessage {
    pub headers: Vec<Pair>,
    pub body: Vec<u8>,
}

/// How the TUI edits one field of a plugin request kind (design D7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldEditor {
    Text,
    /// Code, highlighted as the language named.
    Code(String),
    KeyValue,
    Select(Vec<String>),
}

/// One field of a plugin kind's pane: the request file key it edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneField {
    pub name: String,
    pub label: String,
    pub editor: FieldEditor,
}

/// How the TUI shows a plugin kind's response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneView {
    Raw,
    JsonTree,
    Table,
}

/// The declarative TUI pane of a plugin kind: plugins never draw.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginPane {
    pub fields: Vec<PaneField>,
    pub views: Vec<PaneView>,
}

/// A request kind a plugin registers (plugins spec, "Request kind
/// extension point").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginKind {
    /// The `kind:` of its request files.
    pub name: String,
    /// The workspace name of the plugin that serves it.
    pub plugin: String,
    pub pane: PluginPane,
}

/// What executing a plugin kind request did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KindOutcome {
    /// What was sent, in one line.
    pub summary: String,
    pub request_body: Vec<u8>,
    pub status: Option<u16>,
    pub headers: Vec<Pair>,
    pub body: Vec<u8>,
}

/// A file an importer plugin returned: a path relative to the import
/// folder (or to `environments/`) and its text.
pub type ImportedFile = (String, String);

/// What an importer plugin converted a source into. The host validates and
/// writes it; the plugin writes nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportedFiles {
    /// The workspace name of the plugin that converted it.
    pub plugin: String,
    pub requests: Vec<ImportedFile>,
    pub environments: Vec<ImportedFile>,
}

/// The extension points of a trusted, loaded plugin set. Every error is a
/// message that already names the plugin (`plugin <name>: ...`) and is
/// redacted.
pub trait Extensions: Send + Sync {
    /// `{{$<plugin>.<name> args...}}`.
    fn generate(&self, plugin: &str, name: &str, args: &[String]) -> Result<String, String>;
    /// `auth: {type: plugin, scheme: <plugin>/<scheme>, ...}`.
    fn sign(&self, scheme: &str, request: &SignRequest) -> Result<SignOutput, String>;
    /// `- plugin: {fn: <plugin>/<fn>, ...args}`; `args` is a JSON object.
    fn check(&self, function: &str, exchange: &PluginExchange, args: &str) -> PluginVerdict;
    /// `--reporter <plugin>/<format>:path` and `report render --format
    /// <plugin>/<format>`: the bytes to write for the redacted
    /// `rustman.report/v1` document `report`.
    fn render(&self, format: &str, report: &str) -> Result<Vec<u8>, String> {
        let _ = report;
        Err(format!("report format {format}: no plugin renders it"))
    }
    /// `transform: {request: [<plugin>/<name>]}`: the request's headers and
    /// body after the transformer. It runs after interpolation and before
    /// auth; `url` is the request's URL.
    fn transform_request(
        &self,
        name: &str,
        url: &str,
        message: &PluginMessage,
    ) -> Result<PluginMessage, String> {
        let _ = (url, message);
        Err(format!("transformer {name}: no plugin provides it"))
    }
    /// `transform: {response: [<plugin>/<name>]}`: the response's headers
    /// and body after the transformer, before assertions and captures.
    fn transform_response(
        &self,
        name: &str,
        status: u16,
        message: &PluginMessage,
    ) -> Result<PluginMessage, String> {
        let _ = (status, message);
        Err(format!("transformer {name}: no plugin provides it"))
    }
    /// The request kinds the plugins register.
    fn kinds(&self) -> Vec<PluginKind> {
        Vec::new()
    }
    /// Check the fields of a `kind: <kind>` request file (a JSON object
    /// without the common fields) against the kind's JSON Schema.
    fn validate_kind(&self, kind: &str, fields: &serde_json::Value) -> Result<(), String> {
        let _ = fields;
        Err(format!("unknown request kind '{kind}'"))
    }
    /// Execute a resolved request of a plugin kind: `request` is the
    /// request file as JSON, its variables substituted.
    fn execute_kind(&self, kind: &str, request: &serde_json::Value) -> Result<KindOutcome, String> {
        let _ = request;
        Err(format!("unknown request kind '{kind}'"))
    }
    /// The formats importer plugins register for `rustman import`.
    fn import_formats(&self) -> Vec<String> {
        Vec::new()
    }
    /// `rustman import <format> <src>`: what the importer plugin converted
    /// `source` (a workspace-relative path its `fs:read` grants cover) into.
    fn import(&self, format: &str, source: &str) -> Result<ImportedFiles, String> {
        let _ = source;
        Err(format!("unknown import format '{format}'"))
    }
    /// A redactor of every secret of the plugin session. A plugin granted
    /// a secret may put it in anything it returns (a header, a generated
    /// value, a body), so every sink that records plugin output redacts
    /// all of them.
    fn redactor(&self) -> Redactor {
        Redactor::new()
    }
    /// Secret values a send resolved after the plugins were loaded
    /// (provider values, captured and data-row secrets), by the name they
    /// are redacted as: from now on they are secrets of the plugin
    /// session, so a plugin sees one in clear only with
    /// `secrets:read:<name>` and everything it returns is redacted of it.
    fn learn_secrets(&self, secrets: &[crate::Secret<String>]) {
        let _ = secrets;
    }
}

/// Check the requests of `workspace` loaded as opaque (its kind is not
/// built in): each must be a kind a plugin of `extensions` registers, and
/// its fields must match that kind's schema. Errors name the file and the
/// kind, like any request file that does not load.
pub fn check_kinds(
    workspace: &crate::Workspace,
    extensions: Option<&dyn Extensions>,
) -> Result<(), crate::ConfigError> {
    let kinds: Vec<String> = extensions
        .map(|e| e.kinds().into_iter().map(|k| k.name).collect())
        .unwrap_or_default();
    let mut errors = Vec::new();
    for request in workspace.requests() {
        let crate::model::Request::Opaque(opaque) = request.request() else {
            continue;
        };
        let file = &request.entry.path;
        let position = Some(opaque.kind_position());
        let checked = match extensions.filter(|_| kinds.contains(&opaque.kind)) {
            Some(extensions) => opaque
                .kind_fields(file)
                .map_err(|e| e.to_string())
                .and_then(|fields| extensions.validate_kind(&opaque.kind, &fields)),
            None => Err(unknown_kind(&opaque.kind, &kinds)),
        };
        if let Err(message) = checked {
            errors.push(crate::LoadError::new(file, position, message));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(crate::ConfigError::Invalid(errors))
    }
}

fn unknown_kind(kind: &str, plugin_kinds: &[String]) -> String {
    let mut expected: Vec<&str> = crate::model::BUILTIN_KINDS.to_vec();
    expected.extend(plugin_kinds.iter().map(String::as_str));
    format!(
        "unknown request kind '{kind}' (expected one of: {})",
        expected.join(", ")
    )
}

/// Split `<plugin>/<name>`; `None` unless both parts are non-empty.
pub fn split_qualified(name: &str) -> Option<(&str, &str)> {
    let (plugin, item) = name.split_once('/')?;
    (!plugin.is_empty() && !item.is_empty() && !item.contains('/')).then_some((plugin, item))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_kinds_are_the_registry_defaults() {
        let names = crate::model::KindRegistry::default().names();
        assert_eq!(names, crate::model::BUILTIN_KINDS);
    }

    #[test]
    fn unknown_kinds_list_the_plugin_kinds_too() {
        assert_eq!(
            unknown_kind("echo", &["soap".to_owned()]),
            "unknown request kind 'echo' (expected one of: http, graphql, jsonrpc, grpc, mcp, ws, sse, soap)"
        );
    }

    #[test]
    fn qualified_names_have_two_parts() {
        assert_eq!(split_qualified("hmac/v1"), Some(("hmac", "v1")));
        assert_eq!(split_qualified("hmac"), None);
        assert_eq!(split_qualified("/v1"), None);
        assert_eq!(split_qualified("hmac/"), None);
        assert_eq!(split_qualified("a/b/c"), None);
    }
}
