//! The run's span export (`[trace.export]`): a client span per exchange,
//! queued on the collector's exporter when the `otlp` feature is built.

#[cfg(feature = "otlp")]
use std::sync::Arc;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use rustman_core::Redactor;
use rustman_core::run::RequestRecord;
use rustman_core::trace::{ClientSpan, ExportTarget, TraceConfig};
use rustman_core::vars::ResolvedRequest;

use crate::exchange::Exchange;

/// How long `rustman send` and `rustman run` wait for the last spans.
pub const FLUSH_WAIT: Duration = Duration::from_secs(5);

/// Whether this build can export spans (the `otlp` feature).
pub const OTLP_BUILT: bool = cfg!(feature = "otlp");

/// The collector `config` exports to, as `host:port`: `None` when export is
/// off or this build cannot export.
pub fn export_host(config: &TraceConfig) -> Option<String> {
    let target = ExportTarget::from_process_env(config.export.as_ref()?)?;
    OTLP_BUILT.then(|| target.host())
}

/// Send the spans every run of this process queued, waiting at most `wait`
/// per collector, and return the export warnings not taken yet. Front ends
/// that outlive their runners (the TUI) call it when they quit; a build
/// without the `otlp` feature has nothing to send.
pub fn flush_all_exports(wait: Duration) -> Vec<String> {
    #[cfg(feature = "otlp")]
    let warnings = crate::otlp::flush_all(wait);
    #[cfg(not(feature = "otlp"))]
    let warnings = {
        let _ = wait;
        Vec::new()
    };
    warnings
}

/// The span export of one run, or of one exploration, whose probes go
/// through a governed sender rather than a runner: a client span per
/// exchange, queued on the collector's exporter (shared by every export of
/// the process to that collector).
#[derive(Default)]
pub struct SpanExport {
    #[cfg(feature = "otlp")]
    exporter: Option<Arc<crate::otlp::Exporter>>,
    warnings: Mutex<Vec<String>>,
}

impl SpanExport {
    /// The export `config` asks for.
    pub fn new(config: &TraceConfig) -> Self {
        let target = config
            .export
            .as_ref()
            .and_then(ExportTarget::from_process_env);
        let Some(target) = target else {
            return Self::default();
        };
        Self::to(&target)
    }

    #[cfg(feature = "otlp")]
    fn to(target: &ExportTarget) -> Self {
        Self {
            exporter: Some(crate::otlp::exporter(target)),
            warnings: Mutex::default(),
        }
    }

    #[cfg(not(feature = "otlp"))]
    fn to(_: &ExportTarget) -> Self {
        let warning =
            "trace.export: this build cannot export spans (build rustman with the `otlp` feature)";
        Self {
            warnings: Mutex::new(vec![warning.to_owned()]),
        }
    }

    /// Queue the span of `record` (already redacted), if it has one.
    pub fn record(&self, record: &RequestRecord) {
        #[cfg(feature = "otlp")]
        if let (Some(exporter), Some(span)) = (&self.exporter, ClientSpan::from_record(record)) {
            exporter.record(span);
        }
        #[cfg(not(feature = "otlp"))]
        let _ = (record, ClientSpan::from_record);
    }

    /// Queue the span of `exchange`, sent for `request` outside a runner
    /// (an exploration probe), with what `redactor` knows replaced; `id`
    /// becomes its `rustman.request.id`. A no-op without export.
    pub fn record_exchange(
        &self,
        id: &str,
        request: &ResolvedRequest,
        exchange: &Exchange,
        redactor: &Redactor,
    ) {
        #[cfg(feature = "otlp")]
        if self.exporter.is_some() {
            let mut record = RequestRecord::new(id.to_owned(), id.to_owned(), String::new());
            record.exchange = Some(super::record::exchange_record(request, exchange, redactor));
            record.trace = super::record::trace_record(exchange, None, redactor);
            self.record(&record);
        }
        #[cfg(not(feature = "otlp"))]
        let _ = (id, request, exchange, redactor);
    }

    /// Send the queued spans, waiting at most `wait`.
    pub fn flush(&self, wait: Duration) {
        #[cfg(feature = "otlp")]
        if let Some(exporter) = &self.exporter
            && !exporter.flush(wait)
        {
            tracing::debug!("the trace export did not finish within {wait:?}");
        }
        #[cfg(not(feature = "otlp"))]
        let _ = wait;
    }

    /// The warnings since the last call: a build without export, and
    /// collector failures (at most one a minute).
    pub fn take_warnings(&self) -> Vec<String> {
        let warnings =
            std::mem::take(&mut *self.warnings.lock().unwrap_or_else(PoisonError::into_inner));
        #[cfg(feature = "otlp")]
        let warnings = warnings
            .into_iter()
            .chain(self.exporter.iter().flat_map(|e| e.take_warnings()))
            .collect();
        warnings
    }
}
