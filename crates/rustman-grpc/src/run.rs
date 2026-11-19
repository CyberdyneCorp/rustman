//! Sending a grpc request of a workspace end to end, the one path for
//! `rustman run`, `rustman grpc call` and the other front ends: load the
//! schema (local files, or reflection through the descriptor cache),
//! prepare the call, and send it with the deadline chain applied.
//!
//! [`GrpcRunner`] plugs this into the `rustman-http` runner, which resolves
//! the request, evaluates assertions and captures and records history.

use std::sync::Arc;

use prost_reflect::MethodDescriptor;

use rustman_core::vars::ResolvedGrpc;
use rustman_http::SendOptions;
use rustman_http::runner::{GrpcCall, GrpcExecutor, GrpcFailure, GrpcFuture, GrpcOutcome};

use crate::call::{CallOptions, PreparedCall, StreamBounds, effective_deadline};
use crate::connect::Remote;
use crate::error::{EXIT_CONFIG, GrpcError};
use crate::exchange::GrpcExchange;
use crate::interactive::Interactive;
use crate::schema::{CachePolicy, DescriptorCache, Schema, SchemaSource, load_schema};

/// Sends the grpc requests of a run.
#[derive(Clone, Debug, Default)]
pub struct GrpcRunner {
    /// Response streams run until the server ends them or the deadline
    /// expires, instead of stopping at the request's `stream` bounds.
    pub unbounded: bool,
    /// Ask the server for its schema even when the cache has it.
    pub refresh: bool,
    /// A front end watches the timeline live and may send further bidi
    /// messages (the TUI).
    pub interactive: Option<Arc<Interactive>>,
}

impl GrpcExecutor for GrpcRunner {
    fn execute<'a>(&'a self, call: GrpcCall<'a>) -> GrpcFuture<'a> {
        Box::pin(self.run(call, |_| Ok(())))
    }
}

impl GrpcRunner {
    /// Send `call`; `check` may refuse its method once the schema is
    /// known, before anything is sent.
    pub async fn run(
        &self,
        call: GrpcCall<'_>,
        check: impl FnOnce(&MethodDescriptor) -> Result<(), GrpcError>,
    ) -> Result<GrpcOutcome, GrpcFailure> {
        let redactor = call.resolved.redactor();
        let failure = |e: GrpcError| GrpcFailure {
            config: e.exit_code() == EXIT_CONFIG,
            message: e.redacted(&redactor).to_string(),
        };
        let (schema, remote) = self.schema(&call).await.map_err(failure)?;
        let method = schema.method(&call.resolved.method).map_err(failure)?;
        check(&method).map_err(failure)?;
        let exchange = self.send(&call, &schema, remote).await.map_err(failure)?;
        Ok(outcome(&exchange, schema.notice()))
    }

    /// The schema of the call and the remote it goes to.
    async fn schema(&self, call: &GrpcCall<'_>) -> Result<(Schema, Remote), GrpcError> {
        let cache = call
            .in_workspace
            .then(|| DescriptorCache::for_workspace(call.root));
        schema_for(call.resolved, call.options, cache, self.refresh).await
    }

    async fn send(
        &self,
        call: &GrpcCall<'_>,
        schema: &Schema,
        remote: Remote,
    ) -> Result<GrpcExchange, GrpcError> {
        let resolved = call.resolved;
        let prepared = PreparedCall::new(resolved, schema, remote)?;
        // The run's timeout: `--timeout`, else `[defaults] timeout_ms`.
        let run_timeout = u64::try_from(call.options.timeout.as_millis()).ok();
        let options = CallOptions {
            deadline: effective_deadline(resolved.deadline_ms, run_timeout),
            bounds: (!self.unbounded).then(|| StreamBounds::of(&resolved.stream)),
            http: call.options.clone(),
        };
        match &self.interactive {
            Some(interactive) => {
                let redactor = resolved.redactor();
                interactive.send(&prepared, &options, &redactor).await
            }
            None => prepared.send(&options).await,
        }
    }
}

/// The schema of `resolved` and the remote it goes to: its local files,
/// else reflection through `cache` (asking the server again with
/// `refresh`).
pub async fn schema_for(
    resolved: &ResolvedGrpc,
    options: &SendOptions,
    cache: Option<DescriptorCache>,
    refresh: bool,
) -> Result<(Schema, Remote), GrpcError> {
    let remote = remote(resolved, options)?;
    let policy = CachePolicy {
        cache,
        refresh,
        redactor: resolved.redactor(),
    };
    let schema = load_schema(&SchemaSource::of(&resolved.schema), &remote, &policy).await?;
    Ok((schema, remote))
}

/// The remote of `resolved`, with the TLS settings of `options` and its
/// timeout (`--timeout`, else `[defaults] timeout_ms`) on reflection.
pub fn remote(resolved: &ResolvedGrpc, options: &SendOptions) -> Result<Remote, GrpcError> {
    Ok(Remote::for_request(resolved)?
        .with_tls_settings(options.tls.clone())
        .with_schema_timeout(options.timeout))
}

/// What the runner checks and records of `exchange`.
fn outcome(exchange: &GrpcExchange, notice: Option<String>) -> GrpcOutcome {
    let error = exchange
        .is_request_error()
        .then(|| format!("grpc: {}", exchange.status.label()));
    GrpcOutcome {
        view: exchange.view(),
        error,
        warnings: notice.into_iter().collect(),
        credentials: exchange.credentials(),
        stopped: exchange.stopped.is_some(),
        record: serde_json::to_value(exchange).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::browse::CallType;
    use crate::exchange::GrpcTimings;
    use crate::status::GrpcStatus;

    fn exchange(code: i32) -> GrpcExchange {
        GrpcExchange {
            protocol: "grpc",
            target: "grpc://h:1".into(),
            method: "a.S/M".into(),
            call_type: CallType::Unary,
            transport: "grpc",
            status: GrpcStatus::new(code, "late"),
            headers: Vec::new(),
            trailers: Vec::new(),
            messages: vec![json!({"id": "p-1"})],
            timeline: Vec::new(),
            timings: GrpcTimings {
                total: Duration::from_millis(5),
                ..GrpcTimings::default()
            },
            stopped: None,
        }
    }

    #[test]
    fn an_expired_deadline_is_a_request_error() {
        let late = outcome(&exchange(4), Some("stale".into()));
        assert_eq!(late.error.as_deref(), Some("grpc: DEADLINE_EXCEEDED (4)"));
        assert_eq!(late.warnings, ["stale"]);
        assert_eq!(late.record["protocol"], "grpc");
        let missing = outcome(&exchange(5), None);
        assert!(missing.error.is_none());
        assert_eq!(missing.view.grpc.unwrap().code, 5);
    }
}
