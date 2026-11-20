//! What plugins do in a run (add-plugin-system): response transformers,
//! applied before assertions and captures, and requests of plugin kinds,
//! executed by the plugin that registers the kind.

use bytes::Bytes;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use rustman_core::model::RequestSettings;
use rustman_core::model::{OpaqueRequest, Transform};
use rustman_core::plugins::{KindOutcome, PluginMessage};
use rustman_core::run::{
    HeaderRecord, PluginKindRecord, RequestRecord, RequestStatus, SkipReason, TransformRecord,
};
use rustman_core::vars::{
    Resolved, ResolvedBody, ResolvedRequest, TlsFiles, UsedSecret, resolve_value,
};
use rustman_core::workspace::RequestRef;

use super::{Runner, Sent};
use crate::engine::header_pairs;
use crate::exchange::Exchange;

impl Runner<'_> {
    /// Run the response transformers of `transform` over `exchange`, which
    /// then holds the transformed response. The record keeps the body as
    /// received. A transformer failure is the request's error.
    pub(super) fn transform_response(
        &self,
        transform: &Transform,
        exchange: &mut Exchange,
    ) -> Result<TransformRecord, String> {
        let mut record = TransformRecord {
            request: transform.request.clone(),
            response: transform.response.clone(),
            ..TransformRecord::default()
        };
        if transform.response.is_empty() {
            return Ok(record);
        }
        let Some(plugins) = self.options.plugins.as_deref() else {
            return Err(format!(
                "transformer {}: workspace plugins are not loaded",
                transform.response.join(", ")
            ));
        };
        if exchange.spill.is_some() {
            return Err("response transformers need the whole body, which exceeded \
                        max_body_bytes"
                .to_owned());
        }
        record.original_body =
            (!exchange.binary).then(|| String::from_utf8_lossy(&exchange.body).into_owned());
        record.original_body_bytes = Some(exchange.size_bytes);
        let mut message = PluginMessage {
            headers: header_pairs(&exchange.headers),
            body: exchange.body.to_vec(),
        };
        for name in &transform.response {
            message = plugins.transform_response(name, exchange.status, &message)?;
        }
        replace_response(exchange, message);
        Ok(record)
    }

    /// Execute `opaque`, a request of a plugin kind: its fields are
    /// interpolated, the run's [`super::SendGate`] (the agent interface)
    /// admits it, then the plugin that registers the kind runs it. A kind
    /// no loaded plugin registers is skipped as unsupported.
    pub(super) async fn execute_plugin_kind(
        &self,
        request: &RequestRef<'_>,
        opaque: &OpaqueRequest,
        record: RequestRecord,
        iteration: u32,
    ) -> Sent {
        let plugins = self.options.plugins.as_deref();
        let Some(plugins) = plugins.filter(|p| p.kinds().iter().any(|k| k.name == opaque.kind))
        else {
            return Sent::skipped(record, SkipReason::UnsupportedKind);
        };
        let captured = self.lock_captures().clone();
        let resolver = self.resolver(request, &captured, iteration);
        let resolved = opaque
            .to_json(&request.entry.path)
            .map_err(|e| e.to_string())
            .and_then(|value| resolve_value(&value, &resolver).map_err(|e| e.to_string()));
        let (value, secrets) = match resolved {
            Ok(resolved) => resolved,
            Err(e) => return Sent::errored(record, e),
        };
        self.remember(&secrets);
        let sent_request = kind_request(request, &opaque.kind, value.clone(), secrets);
        if let Some(gate) = self.options.gate
            && let Err(e) = gate.authorize_kind(&sent_request).await
        {
            return Sent::errored(record, e.to_string()).with_resolved(sent_request);
        }
        let sent = match plugins.execute_kind(&opaque.kind, &value) {
            Ok(outcome) => {
                let mut record = record;
                record.status = RequestStatus::Passed;
                record.plugin_kind = Some(kind_record(&opaque.kind, outcome));
                Sent::from_record(record)
            }
            Err(e) => Sent::errored(record, e),
        };
        sent.with_resolved(sent_request)
    }
}

/// What history records as sent for a plugin kind request: the kind as
/// the method, `<kind>:<request id>` as the URL and the resolved fields as
/// a JSON body. It is never replayed.
fn kind_request(
    request: &RequestRef<'_>,
    kind: &str,
    fields: serde_json::Value,
    secrets: Vec<UsedSecret>,
) -> ResolvedRequest {
    ResolvedRequest {
        method: kind.to_owned(),
        url: Resolved::literal(format!("{kind}:{}", request.entry.id())),
        query: Vec::new(),
        headers: Vec::new(),
        body: ResolvedBody::Json(fields),
        auth: None,
        settings: RequestSettings::default(),
        tls: TlsFiles::default(),
        secrets,
    }
}

/// Put the transformed headers and body in `exchange`. A header the
/// transformer returned that is not valid HTTP is dropped.
fn replace_response(exchange: &mut Exchange, message: PluginMessage) {
    let mut headers = HeaderMap::new();
    for (name, value) in &message.headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.append(name, value);
        }
    }
    exchange.headers = headers;
    exchange.binary = std::str::from_utf8(&message.body).is_err();
    exchange.size_bytes = message.body.len() as u64;
    exchange.body = Bytes::from(message.body);
}

fn kind_record(kind: &str, outcome: KindOutcome) -> PluginKindRecord {
    let text = |bytes: &[u8]| std::str::from_utf8(bytes).ok().map(str::to_owned);
    PluginKindRecord {
        kind: kind.to_owned(),
        summary: outcome.summary,
        request_body: text(&outcome.request_body).filter(|b| !b.is_empty()),
        status: outcome.status,
        headers: outcome
            .headers
            .into_iter()
            .map(|(name, value)| HeaderRecord { name, value })
            .collect(),
        body: text(&outcome.body),
        body_bytes: outcome.body.len() as u64,
    }
}
