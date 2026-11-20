//! A verified plugin, compiled and ready to call (plugins spec, "Resource
//! limits" and "Plugin fault isolation"; design D4).
//!
//! One instance per plugin lives in its own store; calls into it are
//! serialized. A trap, a limit breach or an invalid return value fails
//! only the call that caused it, and the instance is recreated before the
//! next call.

use std::sync::{Arc, Mutex, PoisonError};

use wasmtime::component::{Component, Instance, InstancePre};
use wasmtime::{Store, Trap};

use crate::bindings::{assertion, auth, generator, importer, kind, report, transformer};
use crate::error::LoadError;
use crate::extension::ExtensionPoint;
use crate::grant::Grant;
use crate::limits::Limits;
use crate::sandbox::{Exceeded, HostContext, HostState, PluginLog, engine, linker};
use crate::workspace::VerifiedPlugin;

pub use crate::bindings::assertion::exports::rustman::plugin::assertion_fn::{Exchange, Verdict};
pub use crate::bindings::auth::exports::rustman::plugin::auth_scheme::{AuthOutput, AuthRequest};
pub use crate::bindings::host::rustman::plugin::types::{Header, HostError, Param};
pub use crate::bindings::importer::exports::rustman::plugin::format_importer::{
    ImportOutput, ImportedFile,
};
pub use crate::bindings::kind::exports::rustman::plugin::request_kind::{
    Editor, Pane, PaneField, ResponseView,
};
pub use crate::bindings::kind::exports::rustman::plugin::request_kind::{KindExchange, KindInfo};
pub use crate::bindings::transformer::exports::rustman::plugin::body_transformer::Message;

/// Why a call into a plugin failed. Shown as the spec words it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    #[error("plugin {plugin}: execution time limit exceeded")]
    Cpu { plugin: String },
    #[error("plugin {plugin}: memory limit exceeded")]
    Memory { plugin: String },
    #[error("plugin {plugin}: time limit exceeded")]
    Time { plugin: String },
    #[error("plugin {plugin} trapped: {message}")]
    Trap { plugin: String, message: String },
    /// The plugin returned an error, or does not implement the call.
    #[error("plugin {plugin}: {message}")]
    Failed { plugin: String, message: String },
}

/// The exports of each extension point the manifest declares.
#[derive(Clone, Default)]
struct Exports {
    auth: Option<auth::AuthIndices>,
    assertion: Option<assertion::AssertionIndices>,
    generator: Option<generator::GeneratorIndices>,
    transformer: Option<transformer::TransformerIndices>,
    importer: Option<importer::ImporterIndices>,
    kind: Option<kind::KindIndices>,
    report: Option<report::ReportIndices>,
}

/// A live instance and its store.
struct Live {
    store: Store<HostState>,
    instance: Instance,
}

/// A plugin ready to call.
pub struct Plugin {
    name: String,
    grants: Arc<[Grant]>,
    limits: Limits,
    context: Arc<HostContext>,
    pre: InstancePre<HostState>,
    exports: Exports,
    live: Mutex<Option<Live>>,
}

impl std::fmt::Debug for Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugin")
            .field("name", &self.name)
            .field("grants", &self.grants)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// The most compiled components kept for reuse.
const MAX_COMPILED: usize = 32;

/// The component of `wasm`, compiled once per process: a session that
/// opens its plugins again (the TUI, on every send) reuses the machine
/// code. Keyed by the SHA-256 of the bytes, so a changed plugin compiles
/// anew.
fn compile(wasm: &[u8]) -> wasmtime::Result<Component> {
    static COMPILED: std::sync::OnceLock<Mutex<std::collections::HashMap<String, Component>>> =
        std::sync::OnceLock::new();
    let cache = COMPILED.get_or_init(Default::default);
    let key = crate::pin::sha256_hex(wasm);
    let cached = cache
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
        .cloned();
    if let Some(component) = cached {
        return Ok(component);
    }
    let component = Component::new(engine(), wasm)?;
    let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if cache.len() >= MAX_COMPILED {
        cache.clear();
    }
    cache.insert(key, component.clone());
    Ok(component)
}

impl Plugin {
    /// Compile `verified` and check that it imports nothing but the host
    /// imports and exports every extension point its manifest declares.
    /// Nothing runs until the first call.
    pub fn new(verified: &VerifiedPlugin, context: Arc<HostContext>) -> Result<Self, LoadError> {
        let name = verified.name();
        let component = compile(&verified.wasm).map_err(|e| {
            LoadError::invalid(name, format!("not a valid component: {}", root_message(&e)))
        })?;
        let pre = linker().instantiate_pre(&component).map_err(|e| {
            let message = format!(
                "{} (plugins only get the rustman:plugin host imports, no WASI)",
                root_message(&e)
            );
            LoadError::invalid(name, message)
        })?;
        let mut exports = Exports::default();
        for point in &verified.manifest.extension_points {
            exports.add(*point, &pre).map_err(|e| {
                let message = format!(
                    "declares extension point '{point}' but does not export rustman:plugin/{}: {}",
                    point.interface(),
                    root_message(&e)
                );
                LoadError::invalid(name, message)
            })?;
        }
        Ok(Self {
            name: name.to_owned(),
            grants: verified.grants.clone().into(),
            limits: verified.declaration.limits,
            context,
            pre,
            exports,
            live: Mutex::new(None),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn grants(&self) -> &[Grant] {
        &self.grants
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Whether the plugin's manifest declares `point` (and the component
    /// exports it).
    pub fn has(&self, point: ExtensionPoint) -> bool {
        let exports = &self.exports;
        match point {
            ExtensionPoint::Auth => exports.auth.is_some(),
            ExtensionPoint::Assertion => exports.assertion.is_some(),
            ExtensionPoint::Generator => exports.generator.is_some(),
            ExtensionPoint::Transformer => exports.transformer.is_some(),
            ExtensionPoint::Importer => exports.importer.is_some(),
            ExtensionPoint::Kind => exports.kind.is_some(),
            ExtensionPoint::Report => exports.report.is_some(),
        }
    }

    /// A redactor of the secrets this plugin may not read: apply it to
    /// every value handed to the plugin, so it sees `«redacted:<name>»`.
    pub fn input_redactor(&self) -> rustman_core::Redactor {
        let granted = |name: &str| self.grants.contains(&Grant::SecretsRead(name.to_owned()));
        self.context.input_redactor(granted)
    }

    /// A redactor of every secret of the session: apply it to everything
    /// the plugin returns before it is shown or recorded.
    pub fn output_redactor(&self) -> rustman_core::Redactor {
        self.context.redactor()
    }

    /// What the plugin logged since the last call to this, redacted.
    pub fn take_logs(&self) -> Vec<PluginLog> {
        self.with_live(|live| std::mem::take(&mut live.store.data_mut().logs))
    }

    /// The host calls refused since the last call to this, as
    /// `capability_denied: <detail>`.
    pub fn take_denials(&self) -> Vec<String> {
        self.with_live(|live| std::mem::take(&mut live.store.data_mut().denials))
    }

    fn with_live<R: Default>(&self, f: impl FnOnce(&mut Live) -> R) -> R {
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        live.as_mut().map(f).unwrap_or_default()
    }

    /// Run `f` against the instance, creating it first when needed, under
    /// the CPU budget. Any failure drops the instance.
    fn call<R>(
        &self,
        f: impl FnOnce(&mut Store<HostState>, &Instance) -> wasmtime::Result<R>,
    ) -> Result<R, CallError> {
        let mut slot = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(self.instantiate().map_err(|e| self.classify(&e, None))?);
        }
        let live = slot.as_mut().expect("the instance was just created");
        let ticks = live.store.data_mut().meter.start();
        live.store.set_epoch_deadline(ticks);
        let result = f(&mut live.store, &live.instance);
        result.map_err(|e| {
            let state = slot.take().map(|live| live.store.into_data());
            self.classify(&e, state.as_ref())
        })
    }

    fn instantiate(&self) -> wasmtime::Result<Live> {
        let state = HostState::new(
            &self.name,
            self.grants.clone(),
            self.limits,
            self.context.clone(),
        );
        let mut store = Store::new(engine(), state);
        store.limiter(|state| &mut state.limiter);
        store.epoch_deadline_callback(|mut store| store.data_mut().meter.on_deadline());
        let ticks = store.data_mut().meter.start();
        store.set_epoch_deadline(ticks);
        let instance = self.pre.instantiate(&mut store)?;
        Ok(Live { store, instance })
    }

    fn classify(&self, error: &wasmtime::Error, state: Option<&HostState>) -> CallError {
        let plugin = self.name.clone();
        let exceeded = error.downcast_ref::<Exceeded>().copied().or_else(|| {
            let state = state?;
            if state.limiter.exceeded() {
                Some(Exceeded::Memory)
            } else {
                state.meter.exceeded()
            }
        });
        match exceeded {
            Some(Exceeded::Cpu) => CallError::Cpu { plugin },
            Some(Exceeded::Memory) => CallError::Memory { plugin },
            Some(Exceeded::Time) => CallError::Time { plugin },
            None => {
                let message = match error.downcast_ref::<Trap>() {
                    Some(trap) => trap.to_string(),
                    None => root_message(error),
                };
                let redactor = self.context.redactor();
                let message = redactor.text(&message).into_owned();
                CallError::Trap { plugin, message }
            }
        }
    }

    /// A plugin's own error message, redacted.
    fn failed(&self, message: &str) -> CallError {
        let message = self.context.redactor().text(message).into_owned();
        CallError::Failed {
            plugin: self.name.clone(),
            message,
        }
    }

    fn missing(&self, point: ExtensionPoint) -> CallError {
        self.failed(&format!("does not declare extension point '{point}'"))
    }
}

impl Exports {
    fn add(&mut self, point: ExtensionPoint, pre: &InstancePre<HostState>) -> wasmtime::Result<()> {
        match point {
            ExtensionPoint::Auth => self.auth = Some(auth::AuthIndices::new(pre)?),
            ExtensionPoint::Assertion => {
                self.assertion = Some(assertion::AssertionIndices::new(pre)?);
            }
            ExtensionPoint::Generator => {
                self.generator = Some(generator::GeneratorIndices::new(pre)?);
            }
            ExtensionPoint::Transformer => {
                self.transformer = Some(transformer::TransformerIndices::new(pre)?);
            }
            ExtensionPoint::Importer => {
                self.importer = Some(importer::ImporterIndices::new(pre)?);
            }
            ExtensionPoint::Kind => self.kind = Some(kind::KindIndices::new(pre)?),
            ExtensionPoint::Report => self.report = Some(report::ReportIndices::new(pre)?),
        }
        Ok(())
    }
}

/// A wasmtime error's chain of messages, without backtraces.
fn root_message(error: &wasmtime::Error) -> String {
    let messages: Vec<String> = error
        .chain()
        .filter_map(|cause| cause.to_string().lines().next().map(str::to_owned))
        .collect();
    messages.join(": ")
}

/// Calls of each extension point. A guest error (`Err(message)`) is a
/// [`CallError::Failed`] with the message redacted.
impl Plugin {
    pub fn auth_schemes(&self) -> Result<Vec<String>, CallError> {
        let indices = self
            .exports
            .auth
            .as_ref()
            .ok_or(self.missing(ExtensionPoint::Auth))?;
        self.call(|store, instance| {
            let world = indices.load(&mut *store, instance)?;
            world.rustman_plugin_auth_scheme().call_schemes(store)
        })
    }

    pub fn sign(&self, request: &AuthRequest) -> Result<AuthOutput, CallError> {
        let indices = self
            .exports
            .auth
            .as_ref()
            .ok_or(self.missing(ExtensionPoint::Auth))?;
        let signed = self.call(|store, instance| {
            let world = indices.load(&mut *store, instance)?;
            world.rustman_plugin_auth_scheme().call_sign(store, request)
        })?;
        signed.map_err(|message| self.failed(&message))
    }

    pub fn assertion_functions(&self) -> Result<Vec<String>, CallError> {
        let indices = self.exports.assertion.as_ref();
        let indices = indices.ok_or(self.missing(ExtensionPoint::Assertion))?;
        self.call(|store, instance| {
            let world = indices.load(&mut *store, instance)?;
            world.rustman_plugin_assertion_fn().call_functions(store)
        })
    }

    pub fn check(
        &self,
        function: &str,
        exchange: &Exchange,
        args: &str,
    ) -> Result<Verdict, CallError> {
        let indices = self.exports.assertion.as_ref();
        let indices = indices.ok_or(self.missing(ExtensionPoint::Assertion))?;
        self.call(|store, instance| {
            let world = indices.load(&mut *store, instance)?;
            world
                .rustman_plugin_assertion_fn()
                .call_check(store, function, exchange, args)
        })
    }

    pub fn generator_names(&self) -> Result<Vec<String>, CallError> {
        let indices = self.exports.generator.as_ref();
        let indices = indices.ok_or(self.missing(ExtensionPoint::Generator))?;
        self.call(|store, instance| {
            let world = indices.load(&mut *store, instance)?;
            world.rustman_plugin_value_generator().call_names(store)
        })
    }

    pub fn generate(&self, name: &str, args: &[String]) -> Result<String, CallError> {
        let indices = self.exports.generator.as_ref();
        let indices = indices.ok_or(self.missing(ExtensionPoint::Generator))?;
        let generated = self.call(|store, instance| {
            let world = indices.load(&mut *store, instance)?;
            world
                .rustman_plugin_value_generator()
                .call_generate(store, name, args)
        })?;
        generated.map_err(|message| self.failed(&message))
    }

    /// The request and the response transformers.
    pub fn transformers(&self) -> Result<(Vec<String>, Vec<String>), CallError> {
        let indices = self.exports.transformer.as_ref();
        let indices = indices.ok_or(self.missing(ExtensionPoint::Transformer))?;
        self.call(|store, instance| {
            let world = indices.load(&mut *store, instance)?;
            let guest = world.rustman_plugin_body_transformer();
            Ok((
                guest.call_request_transformers(&mut *store)?,
                guest.call_response_transformers(store)?,
            ))
        })
    }

    pub fn transform_request(
        &self,
        name: &str,
        url: &str,
        message: &Message,
    ) -> Result<Message, CallError> {
        let indices = self.exports.transformer.as_ref();
        let indices = indices.ok_or(self.missing(ExtensionPoint::Transformer))?;
        let transformed = self.call(|store, instance| {
            let world = indices.load(&mut *store, instance)?;
            world
                .rustman_plugin_body_transformer()
                .call_transform_request(store, name, url, message)
        })?;
        transformed.map_err(|message| self.failed(&message))
    }

    pub fn transform_response(
        &self,
        name: &str,
        status: u16,
        message: &Message,
    ) -> Result<Message, CallError> {
        let indices = self.exports.transformer.as_ref();
        let indices = indices.ok_or(self.missing(ExtensionPoint::Transformer))?;
        let transformed = self.call(|store, instance| {
            let world = indices.load(&mut *store, instance)?;
            world
                .rustman_plugin_body_transformer()
                .call_transform_response(store, name, status, message)
        })?;
        transformed.map_err(|message| self.failed(&message))
    }

    pub fn import_formats(&self) -> Result<Vec<String>, CallError> {
        let indices = self.exports.importer.as_ref();
        let indices = indices.ok_or(self.missing(ExtensionPoint::Importer))?;
        self.call(|store, instance| {
            let world = indices.load(&mut *store, instance)?;
            world.rustman_plugin_format_importer().call_formats(store)
        })
    }

    pub fn convert(
        &self,
        format: &str,
        source_path: &str,
        source: &[u8],
    ) -> Result<ImportOutput, CallError> {
        let indices = self.exports.importer.as_ref();
        let indices = indices.ok_or(self.missing(ExtensionPoint::Importer))?;
        let converted = self.call(|store, instance| {
            let world = indices.load(&mut *store, instance)?;
            world
                .rustman_plugin_format_importer()
                .call_convert(store, format, source_path, source)
        })?;
        converted.map_err(|message| self.failed(&message))
    }

    pub fn kinds(&self) -> Result<Vec<KindInfo>, CallError> {
        let indices = self
            .exports
            .kind
            .as_ref()
            .ok_or(self.missing(ExtensionPoint::Kind))?;
        self.call(|store, instance| {
            let world = indices.load(&mut *store, instance)?;
            world.rustman_plugin_request_kind().call_kinds(store)
        })
    }

    pub fn execute(&self, kind: &str, request: &str) -> Result<KindExchange, CallError> {
        let indices = self
            .exports
            .kind
            .as_ref()
            .ok_or(self.missing(ExtensionPoint::Kind))?;
        let executed = self.call(|store, instance| {
            let world = indices.load(&mut *store, instance)?;
            world
                .rustman_plugin_request_kind()
                .call_execute(store, kind, request)
        })?;
        executed.map_err(|message| self.failed(&message))
    }

    pub fn report_formats(&self) -> Result<Vec<String>, CallError> {
        let indices = self.exports.report.as_ref();
        let indices = indices.ok_or(self.missing(ExtensionPoint::Report))?;
        self.call(|store, instance| {
            let world = indices.load(&mut *store, instance)?;
            world.rustman_plugin_report_renderer().call_formats(store)
        })
    }

    pub fn render(&self, format: &str, report: &str) -> Result<Vec<u8>, CallError> {
        let indices = self.exports.report.as_ref();
        let indices = indices.ok_or(self.missing(ExtensionPoint::Report))?;
        let rendered = self.call(|store, instance| {
            let world = indices.load(&mut *store, instance)?;
            world
                .rustman_plugin_report_renderer()
                .call_render(store, format, report)
        })?;
        rendered.map_err(|message| self.failed(&message))
    }
}
