//! Transformers, request kinds and importers of a loaded plugin set
//! (plugins spec, "Transformer extension point", "Request kind extension
//! point", "Importer extension point").
//!
//! Kinds are registered when the set loads: a built-in name is refused,
//! and each kind's JSON Schema is compiled once. Every value handed to a
//! plugin is redacted of the secrets it holds no grant for; what an
//! importer returns is redacted of every secret of the session, since the
//! host writes it into tracked files.

use rustman_core::model::BUILTIN_KINDS;
use rustman_core::plugins::{
    FieldEditor, ImportedFiles, KindOutcome, PaneField, PaneView, PluginKind, PluginMessage,
    PluginPane,
};
use serde_json::Value;

use crate::bindings::host::rustman::plugin::types::HostError;
use crate::bindings::kind::exports::rustman::plugin::request_kind as wit;
use crate::error::LoadError;
use crate::extension::ExtensionPoint;
use crate::plugin::{Header, KindInfo, Message, Plugin};
use crate::runtime::PluginRuntime;
use crate::sandbox::fs::{FsRefusal, resolve};

/// The most schema violations an error lists.
const MAX_VIOLATIONS: usize = 3;

/// A kind a plugin registered, with its compiled schema.
pub(crate) struct RegisteredKind {
    pub info: PluginKind,
    validator: jsonschema::Validator,
}

impl std::fmt::Debug for RegisteredKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredKind")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// The kinds of every kind plugin of `plugins`, refusing built-in names,
/// names registered twice and invalid schemas.
pub(crate) fn register_kinds<'p>(
    plugins: impl IntoIterator<Item = &'p Plugin>,
) -> Result<Vec<RegisteredKind>, LoadError> {
    let mut kinds: Vec<RegisteredKind> = Vec::new();
    for plugin in plugins {
        if !plugin.has(ExtensionPoint::Kind) {
            continue;
        }
        let infos = plugin
            .kinds()
            .map_err(|e| LoadError::Workspace(e.to_string()))?;
        for info in infos {
            let kind = register(plugin.name(), info, &kinds)?;
            kinds.push(kind);
        }
    }
    Ok(kinds)
}

fn register(
    plugin: &str,
    info: KindInfo,
    existing: &[RegisteredKind],
) -> Result<RegisteredKind, LoadError> {
    let name = info.name;
    if BUILTIN_KINDS.contains(&name.as_str()) {
        return Err(LoadError::invalid(
            plugin,
            format!("kind '{name}' is reserved"),
        ));
    }
    if !valid_kind_name(&name) {
        return Err(LoadError::invalid(
            plugin,
            format!("invalid kind name '{name}' (expected lowercase letters, digits and '-')"),
        ));
    }
    if let Some(other) = existing.iter().find(|k| k.info.name == name) {
        let message = format!(
            "kind '{name}' is already registered by plugin {}",
            other.info.plugin
        );
        return Err(LoadError::invalid(plugin, message));
    }
    let schema: Value = serde_json::from_str(&info.schema).map_err(|e| {
        LoadError::invalid(plugin, format!("kind '{name}': schema is not JSON: {e}"))
    })?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|e| LoadError::invalid(plugin, format!("kind '{name}': invalid schema: {e}")))?;
    Ok(RegisteredKind {
        info: PluginKind {
            name,
            plugin: plugin.to_owned(),
            pane: pane(info.pane),
        },
        validator,
    })
}

fn valid_kind_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn pane(pane: wit::Pane) -> PluginPane {
    PluginPane {
        fields: pane
            .fields
            .into_iter()
            .map(|field| PaneField {
                name: field.name,
                label: field.label,
                editor: match field.editor {
                    wit::Editor::Text => FieldEditor::Text,
                    wit::Editor::Code(language) => FieldEditor::Code(language),
                    wit::Editor::KeyValue => FieldEditor::KeyValue,
                    wit::Editor::Select(options) => FieldEditor::Select(options),
                },
            })
            .collect(),
        views: pane
            .views
            .into_iter()
            .map(|view| match view {
                wit::ResponseView::Raw => PaneView::Raw,
                wit::ResponseView::JsonTree => PaneView::JsonTree,
                wit::ResponseView::Table => PaneView::Table,
            })
            .collect(),
    }
}

impl PluginRuntime {
    fn kind(&self, kind: &str) -> Result<&RegisteredKind, String> {
        self.registered_kinds()
            .iter()
            .find(|k| k.info.name == kind)
            .ok_or_else(|| format!("unknown request kind '{kind}'"))
    }

    /// Check `fields` against the schema of `kind`.
    pub(crate) fn validate_fields(&self, kind: &str, fields: &Value) -> Result<(), String> {
        let registered = self.kind(kind)?;
        let violations: Vec<String> = registered
            .validator
            .iter_errors(fields)
            .take(MAX_VIOLATIONS)
            .map(|e| {
                let path = e.instance_path().as_str();
                let path = if path.is_empty() { "/" } else { path };
                format!("{path}: {e}")
            })
            .collect();
        if violations.is_empty() {
            return Ok(());
        }
        Err(format!(
            "plugin {}: kind '{kind}': {}",
            registered.info.plugin,
            violations.join("; ")
        ))
    }

    /// Run a resolved request of `kind` through the plugin that serves it.
    pub(crate) fn execute_request(
        &self,
        kind: &str,
        request: &Value,
    ) -> Result<KindOutcome, String> {
        let plugin = self.plugin(&self.kind(kind)?.info.plugin)?;
        let request = plugin
            .input_redactor()
            .text(&request.to_string())
            .into_owned();
        let exchange = plugin.execute(kind, &request).map_err(|e| e.to_string())?;
        Ok(KindOutcome {
            summary: exchange.summary,
            request_body: exchange.request_body,
            status: exchange.status,
            headers: pairs(exchange.headers),
            body: exchange.body,
        })
    }

    /// Run the transformer `<plugin>/<name>` on a request.
    pub(crate) fn transform_outgoing(
        &self,
        name: &str,
        url: &str,
        message: &PluginMessage,
    ) -> Result<PluginMessage, String> {
        let (plugin, item) = self.route(name, "transformer")?;
        let redactor = plugin.input_redactor();
        let input = wit_message(message, &redactor);
        let url = redactor.text(url);
        let output = plugin
            .transform_request(item, &url, &input)
            .map_err(|e| e.to_string())?;
        Ok(core_message(output))
    }

    /// Run the transformer `<plugin>/<name>` on a response.
    pub(crate) fn transform_incoming(
        &self,
        name: &str,
        status: u16,
        message: &PluginMessage,
    ) -> Result<PluginMessage, String> {
        let (plugin, item) = self.route(name, "transformer")?;
        let input = wit_message(message, &plugin.input_redactor());
        let output = plugin
            .transform_response(item, status, &input)
            .map_err(|e| e.to_string())?;
        Ok(core_message(output))
    }

    /// The import formats of every importer plugin, as `(format, plugin)`.
    pub(crate) fn formats(&self) -> Vec<(String, &Plugin)> {
        let importers = self.plugins().filter(|p| p.has(ExtensionPoint::Importer));
        importers
            .flat_map(|plugin| {
                let formats = plugin.import_formats().unwrap_or_default();
                formats.into_iter().map(move |format| (format, plugin))
            })
            .collect()
    }

    /// Convert the workspace file `source` with the importer of `format`,
    /// which must hold an `fs:read` grant covering it.
    pub(crate) fn convert_source(
        &self,
        format: &str,
        source: &str,
    ) -> Result<ImportedFiles, String> {
        let formats = self.formats();
        let mut owners = formats.iter().filter(|(f, _)| f == format).map(|(_, p)| p);
        let plugin = owners
            .next()
            .ok_or_else(|| format!("unknown import format '{format}'"))?;
        if let Some(other) = owners.next() {
            return Err(format!(
                "import format '{format}' is registered by plugins {} and {}",
                plugin.name(),
                other.name()
            ));
        }
        let bytes = read_source(self.root(), plugin, source)?;
        let output = plugin
            .convert(format, source, &bytes)
            .map_err(|e| e.to_string())?;
        let redactor = plugin.output_redactor();
        let files = |files: Vec<crate::plugin::ImportedFile>| {
            files
                .into_iter()
                .map(|f| (f.path, redactor.text(&f.content).into_owned()))
                .collect()
        };
        Ok(ImportedFiles {
            plugin: plugin.name().to_owned(),
            requests: files(output.requests),
            environments: files(output.environments),
        })
    }
}

/// The bytes of `source`, which an `fs:read` grant of `plugin` must cover.
/// Like `fs-read`, a source larger than the plugin's memory limit is
/// refused without reading it whole.
fn read_source(root: &std::path::Path, plugin: &Plugin, source: &str) -> Result<Vec<u8>, String> {
    let name = plugin.name();
    let path = resolve(root, plugin.grants(), source).map_err(|refusal| match refusal {
        FsRefusal::Denied => format!("plugin {name}: capability_denied: fs {source}"),
        FsRefusal::Failed(e) => format!("plugin {name}: cannot read {source}: {e}"),
    })?;
    let limit = plugin.limits().memory_bytes() as u64;
    crate::sandbox::read_capped(&path, limit).map_err(|e| {
        let detail = match e {
            HostError::Failed(detail) => detail,
            other => format!("{other:?}"),
        };
        format!("plugin {name}: cannot read {source}: {detail}")
    })
}

fn wit_message(message: &PluginMessage, redactor: &rustman_core::Redactor) -> Message {
    Message {
        headers: message
            .headers
            .iter()
            .map(|(name, value)| Header {
                name: name.clone(),
                value: redactor.text(value).into_owned(),
            })
            .collect(),
        body: redactor.bytes(&message.body).into_owned(),
    }
}

fn core_message(message: Message) -> PluginMessage {
    PluginMessage {
        headers: pairs(message.headers),
        body: message.body,
    }
}

fn pairs(headers: Vec<Header>) -> Vec<(String, String)> {
    headers.into_iter().map(|h| (h.name, h.value)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_names_are_lowercase_words() {
        assert!(valid_kind_name("soap"));
        assert!(valid_kind_name("soap-1"));
        assert!(!valid_kind_name("Soap"));
        assert!(!valid_kind_name("1soap"));
        assert!(!valid_kind_name("so ap"));
        assert!(!valid_kind_name(""));
    }
}
