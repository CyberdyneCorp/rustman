//! A trusted plugin set, compiled and serving the extension points
//! (plugins spec, "Auth scheme extension point", "Assertion extension
//! point", "Value generator extension point"; "Plugin fault isolation").
//!
//! Names are routed by the workspace's plugin name: `hmac/v1` is the `v1`
//! scheme of the plugin declared as `[plugins.hmac]`. Every value handed
//! to a plugin is redacted of the secrets it holds no `secrets:read`
//! grant for, and every message it returns is redacted of all of them.

use std::collections::BTreeMap;
use std::sync::Arc;

use std::path::{Path, PathBuf};

use rustman_core::plugins::{
    Extensions, ImportedFiles, KindOutcome, Pair, PluginExchange, PluginKind, PluginMessage,
    PluginVerdict, SignOutput, SignRequest,
};
use serde_json::Value;

use crate::error::LoadError;
use crate::plugin::{AuthRequest, CallError, Exchange, Header, Param, Plugin, Verdict};
use crate::points::{RegisteredKind, register_kinds};
use crate::sandbox::HostContext;
use crate::workspace::PluginSet;

/// The loaded plugins of a session, by workspace name.
#[derive(Debug, Default)]
pub struct PluginRuntime {
    plugins: BTreeMap<String, Plugin>,
    /// Every secret of the session, which all plugin output is redacted of.
    redactor: rustman_core::Redactor,
    /// The request kinds the plugins register.
    kinds: Vec<RegisteredKind>,
    /// The workspace root, which importers read their sources from.
    root: PathBuf,
}

impl PluginRuntime {
    /// Compile every plugin of `set` for `context`, and register the
    /// request kinds of its kind plugins (the only plugin code that runs
    /// here). A kind with a built-in name is refused.
    pub fn new(set: &PluginSet, context: HostContext) -> Result<Self, LoadError> {
        let redactor = context.redactor();
        let root = context.root.clone();
        let context = Arc::new(context);
        let mut plugins = BTreeMap::new();
        for verified in &set.plugins {
            let plugin = Plugin::new(verified, context.clone())?;
            plugins.insert(verified.name().to_owned(), plugin);
        }
        let kinds = register_kinds(plugins.values())?;
        Ok(Self {
            plugins,
            redactor,
            kinds,
            root,
        })
    }

    pub(crate) fn registered_kinds(&self) -> &[RegisteredKind] {
        &self.kinds
    }

    pub(crate) fn plugins(&self) -> impl Iterator<Item = &Plugin> {
        self.plugins.values()
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    pub fn names(&self) -> Vec<&str> {
        self.plugins.keys().map(String::as_str).collect()
    }

    pub fn get(&self, name: &str) -> Option<&Plugin> {
        self.plugins.get(name)
    }

    /// The plugin and item of `<plugin>/<item>`.
    pub(crate) fn route<'a>(
        &self,
        qualified: &'a str,
        what: &str,
    ) -> Result<(&Plugin, &'a str), String> {
        let Some((plugin, item)) = rustman_core::plugins::split_qualified(qualified) else {
            return Err(format!(
                "invalid {what} '{qualified}' (expected <plugin>/<name>)"
            ));
        };
        let plugin = self.plugin(plugin)?;
        Ok((plugin, item))
    }

    pub(crate) fn plugin(&self, name: &str) -> Result<&Plugin, String> {
        self.plugins
            .get(name)
            .ok_or_else(|| format!("plugin {name} is not loaded"))
    }
}

impl Extensions for PluginRuntime {
    fn generate(&self, plugin: &str, name: &str, args: &[String]) -> Result<String, String> {
        let plugin = self.plugin(plugin)?;
        let redactor = plugin.input_redactor();
        let args: Vec<String> = args.iter().map(|a| redactor.text(a).into_owned()).collect();
        plugin.generate(name, &args).map_err(message)
    }

    fn sign(&self, scheme: &str, request: &SignRequest) -> Result<SignOutput, String> {
        let (plugin, scheme) = self.route(scheme, "auth scheme")?;
        let redactor = plugin.input_redactor();
        let input = AuthRequest {
            scheme: scheme.to_owned(),
            method: request.method.clone(),
            url: redactor.text(&request.url).into_owned(),
            headers: headers(&request.headers, &redactor),
            body: redactor.bytes(&request.body).into_owned(),
            params: request
                .params
                .iter()
                .map(|(name, value)| Param {
                    name: name.clone(),
                    value: redactor.text(value).into_owned(),
                })
                .collect(),
        };
        let output = plugin.sign(&input).map_err(message)?;
        Ok(SignOutput {
            set_headers: output
                .set_headers
                .into_iter()
                .map(|h| (h.name, h.value))
                .collect(),
            add_query: output
                .add_query
                .into_iter()
                .map(|p| (p.name, p.value))
                .collect(),
            url: output.url,
        })
    }

    fn check(&self, function: &str, exchange: &PluginExchange, args: &str) -> PluginVerdict {
        let (plugin, name) = match self.route(function, "assertion function") {
            Ok(routed) => routed,
            Err(e) => return PluginVerdict::Error(e),
        };
        let redactor = plugin.input_redactor();
        let input = Exchange {
            method: exchange.method.clone(),
            url: redactor.text(&exchange.url).into_owned(),
            request_headers: headers(&exchange.request_headers, &redactor),
            request_body: redactor.bytes(&exchange.request_body).into_owned(),
            status: exchange.status,
            response_headers: headers(&exchange.response_headers, &redactor),
            response_body: redactor.bytes(&exchange.response_body).into_owned(),
            duration_ms: exchange.duration_ms,
        };
        let args = redactor.text(args);
        let output = plugin.output_redactor();
        match plugin.check(name, &input, &args) {
            Ok(Verdict::Pass) => PluginVerdict::Pass,
            Ok(Verdict::Fail(m)) => PluginVerdict::Fail(output.text(&m).into_owned()),
            Ok(Verdict::Error(m)) => PluginVerdict::Error(output.text(&m).into_owned()),
            Err(e) => PluginVerdict::Error(e.to_string()),
        }
    }

    fn render(&self, format: &str, report: &str) -> Result<Vec<u8>, String> {
        self.render_report(format, report)
    }

    fn redactor(&self) -> rustman_core::Redactor {
        self.redactor.clone()
    }

    fn transform_request(
        &self,
        name: &str,
        url: &str,
        message: &PluginMessage,
    ) -> Result<PluginMessage, String> {
        self.transform_outgoing(name, url, message)
    }

    fn transform_response(
        &self,
        name: &str,
        status: u16,
        message: &PluginMessage,
    ) -> Result<PluginMessage, String> {
        self.transform_incoming(name, status, message)
    }

    fn kinds(&self) -> Vec<PluginKind> {
        self.kinds.iter().map(|k| k.info.clone()).collect()
    }

    fn validate_kind(&self, kind: &str, fields: &Value) -> Result<(), String> {
        self.validate_fields(kind, fields)
    }

    fn execute_kind(&self, kind: &str, request: &Value) -> Result<KindOutcome, String> {
        self.execute_request(kind, request)
    }

    fn import_formats(&self) -> Vec<String> {
        self.formats()
            .into_iter()
            .map(|(format, _)| format)
            .collect()
    }

    fn import(&self, format: &str, source: &str) -> Result<ImportedFiles, String> {
        self.convert_source(format, source)
    }
}

impl PluginRuntime {
    /// The bytes a report plugin returned for `report`, redacted of the
    /// session's secrets (which leaves them unchanged unless one leaked).
    fn render_report(&self, format: &str, report: &str) -> Result<Vec<u8>, String> {
        let (plugin, name) = self.route(format, "report format")?;
        let report = plugin.input_redactor().text(report).into_owned();
        let bytes = plugin.render(name, &report).map_err(message)?;
        Ok(plugin.output_redactor().bytes(&bytes).into_owned())
    }
}

fn headers(pairs: &[Pair], redactor: &rustman_core::Redactor) -> Vec<Header> {
    pairs
        .iter()
        .map(|(name, value)| Header {
            name: name.clone(),
            value: redactor.text(value).into_owned(),
        })
        .collect()
}

/// A failed call as the error of the operation that made it; already
/// redacted by [`Plugin`].
fn message(error: CallError) -> String {
    error.to_string()
}
