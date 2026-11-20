//! The capability sandbox (plugins spec, "Capability sandbox" and
//! "Resource limits"; design D3, D4).
//!
//! Every plugin instance lives in its own [`wasmtime::Store`] whose data is
//! a [`HostState`]: the plugin's grants, the session it serves, its memory
//! limiter and its CPU meter. The linker holds only the `rustman:plugin`
//! host imports, so a component that imports anything else (any WASI
//! interface) cannot be instantiated: no ambient capability exists.

mod context;
mod engine;
pub(crate) mod fs;
mod host;
mod limits;
pub mod net;
pub mod secrets;

pub use context::{HostContext, HostServices, LogLevel, PluginLog, SystemServices};
pub use engine::{TICK, engine, linker};
pub(crate) use host::read_capped;
pub use host::{
    MAX_DENIALS, MAX_DETAIL_BYTES, MAX_LOG_LINE_BYTES, MAX_LOG_LINES, MAX_RESOURCES,
    MAX_SECRET_NAME_BYTES,
};
pub use limits::{CpuMeter, Exceeded, MemoryLimiter};

/// What the host `http` import sends and returns.
pub use crate::bindings::host::rustman::plugin::http::{
    Request as HttpRequest, Response as HttpResponse,
};

use std::sync::Arc;

use rustman_core::Redactor;
use wasmtime::component::ResourceTable;

use crate::bindings::host::rustman::plugin::types::HostError;
use crate::grant::Grant;
use crate::limits::Limits;

/// The data of one plugin instance's store.
pub struct HostState {
    pub(crate) plugin: String,
    pub(crate) grants: Arc<[Grant]>,
    pub(crate) context: Arc<HostContext>,
    pub(crate) table: ResourceTable,
    pub(crate) limiter: MemoryLimiter,
    pub(crate) meter: CpuMeter,
    /// Redacts every secret of the session from what the plugin logs.
    pub(crate) redactor: Redactor,
    pub(crate) logs: Vec<PluginLog>,
    /// `capability_denied: ...` for each refused host call, redacted; at
    /// most [`MAX_DENIALS`], each detail cut to [`MAX_DETAIL_BYTES`].
    pub(crate) denials: Vec<String>,
}

impl HostState {
    pub fn new(
        plugin: &str,
        grants: Arc<[Grant]>,
        limits: Limits,
        context: Arc<HostContext>,
    ) -> Self {
        let mut table = ResourceTable::new();
        table.set_max_capacity(MAX_RESOURCES);
        Self {
            plugin: plugin.to_owned(),
            redactor: context.redactor(),
            grants,
            context,
            table,
            limiter: MemoryLimiter::new(limits.memory_bytes()),
            meter: CpuMeter::new(limits.cpu()),
            logs: Vec::new(),
            denials: Vec::new(),
        }
    }

    pub(crate) fn has(&self, grant: &Grant) -> bool {
        self.grants.contains(grant)
    }

    /// Refuse a call: the plugin gets `capability-denied(detail)`, shown as
    /// `capability_denied: <detail>`. The detail may come from the guest
    /// (an env name, a host), so it is cut to [`MAX_DETAIL_BYTES`], and
    /// only the first [`MAX_DENIALS`] refusals are kept.
    pub(crate) fn deny(&mut self, mut detail: String) -> HostError {
        host::truncate(&mut detail, MAX_DETAIL_BYTES);
        let message = format!("capability_denied: {detail}");
        let message = self.redactor.text(&message).into_owned();
        tracing::debug!(plugin = %self.plugin, "{message}");
        if self.denials.len() < MAX_DENIALS {
            self.denials.push(message);
        }
        HostError::CapabilityDenied(detail)
    }
}

/// How a host error reads in messages: `capability_denied: net x`.
pub fn describe(error: &HostError) -> String {
    match error {
        HostError::CapabilityDenied(detail) => format!("capability_denied: {detail}"),
        HostError::PolicyDenied(detail) => format!("policy_denied: {detail}"),
        HostError::Failed(detail) => detail.clone(),
    }
}
