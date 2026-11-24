//! What an exchange produced: one result per call, matched by id, and the
//! exchange as a whole (transport, attempts, subscription events).

use std::time::Duration;

use rustman_core::trace::SentTrace;
use rustman_http::Exchange;
use serde_json::{Map, Value};

use crate::envelope::{Envelope, Response, RpcError, check_id, classify, response_id, show_id};
use crate::error::JsonrpcError;
use crate::subscription::SubscriptionRecord;

/// The note shown once when a server answers a batch with one error.
pub const BATCH_REJECTED: &str = "server rejected the batch";

/// How one call ended.
#[derive(Clone, Debug, PartialEq)]
pub enum CallOutcome {
    /// The call succeeded with this `result`.
    Result(Value),
    /// The server answered with an error object.
    Error(RpcError),
    /// A notification: no response is expected.
    Notification,
    /// No usable response: invalid, mismatched or missing.
    Failed(String),
}

/// One call of an exchange.
#[derive(Clone, Debug, PartialEq)]
pub struct CallResult {
    pub method: String,
    /// The id sent; `None` for a notification.
    pub id: Option<Value>,
    /// The envelope sent.
    pub request: Value,
    /// The envelope received, when one answered this call.
    pub response: Option<Value>,
    pub outcome: CallOutcome,
    /// The server answered the whole batch with one error object.
    pub batch_rejected: bool,
}

impl CallResult {
    fn new(envelope: &Envelope, outcome: CallOutcome, response: Option<Value>) -> Self {
        Self {
            method: envelope.value["method"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            id: envelope.id.clone(),
            request: envelope.value.clone(),
            response,
            outcome,
            batch_rejected: false,
        }
    }

    /// Why the call failed: `rpc error <code>: <message>`, or the reason no
    /// usable response came back. `None` for a result or a notification.
    pub fn failure(&self) -> Option<String> {
        match &self.outcome {
            CallOutcome::Result(_) | CallOutcome::Notification => None,
            CallOutcome::Error(error) => Some(error.to_string()),
            CallOutcome::Failed(reason) => Some(reason.clone()),
        }
    }

    /// The document assertions and captures read (design D3): the
    /// response envelope, or an empty object without one.
    pub fn envelope(&self) -> Value {
        self.response
            .clone()
            .unwrap_or_else(|| Value::Object(Map::new()))
    }
}

/// The result of a single call answered by `response`.
pub fn single_result(envelope: &Envelope, response: &Value) -> Result<CallResult, JsonrpcError> {
    if let Some(expected) = &envelope.id {
        check_id(response, expected)?;
    }
    let outcome = match classify(response)? {
        Response::Result(value) => CallOutcome::Result(value),
        Response::Error(error) => CallOutcome::Error(error),
    };
    Ok(CallResult::new(envelope, outcome, Some(response.clone())))
}

/// The result of a notification.
pub fn notification_result(envelope: &Envelope) -> CallResult {
    CallResult::new(envelope, CallOutcome::Notification, None)
}

/// Match a batch `response` to the calls `envelopes` by id, in any order.
/// A single error object instead of an array is the server rejecting the
/// batch: every call fails with that error.
pub fn batch_results(
    envelopes: &[Envelope],
    response: &Value,
) -> Result<Vec<CallResult>, JsonrpcError> {
    let items = match response {
        Value::Array(items) => items,
        single => return rejected_batch(envelopes, single),
    };
    Ok(envelopes
        .iter()
        .map(|envelope| batch_item(envelope, items))
        .collect())
}

fn batch_item(envelope: &Envelope, items: &[Value]) -> CallResult {
    let Some(id) = &envelope.id else {
        return notification_result(envelope);
    };
    let Some(item) = items.iter().find(|item| response_id(item) == id) else {
        let reason = format!("no response for id {}", show_id(id));
        return CallResult::new(envelope, CallOutcome::Failed(reason), None);
    };
    let outcome = match classify(item) {
        Ok(Response::Result(value)) => CallOutcome::Result(value),
        Ok(Response::Error(error)) => CallOutcome::Error(error),
        Err(e) => CallOutcome::Failed(e.to_string()),
    };
    CallResult::new(envelope, outcome, Some(item.clone()))
}

fn rejected_batch(
    envelopes: &[Envelope],
    response: &Value,
) -> Result<Vec<CallResult>, JsonrpcError> {
    let Response::Error(error) = classify(response)? else {
        return Err(JsonrpcError::Invalid(
            "a batch must be answered with an array".to_owned(),
        ));
    };
    Ok(envelopes
        .iter()
        .map(|envelope| {
            let outcome = match envelope.id {
                Some(_) => CallOutcome::Error(error.clone()),
                None => CallOutcome::Notification,
            };
            let mut result = CallResult::new(envelope, outcome, Some(response.clone()));
            result.batch_rejected = true;
            result
        })
        .collect())
}

/// The transport an exchange went over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportKind {
    Http,
    WebSocket,
}

/// One try at sending the request (several with retries).
#[derive(Clone, Debug, PartialEq)]
pub struct Attempt {
    /// The HTTP status, when a response came back.
    pub status: Option<u16>,
    /// Why the attempt did not succeed; redacted.
    pub error: Option<String>,
    pub elapsed: Duration,
    /// The wait before the next attempt, when there was one.
    pub retry_in: Option<Duration>,
}

/// One JSON-RPC exchange: a call, a batch or a subscription.
#[derive(Clone, Debug)]
pub struct RpcExchange {
    pub transport: TransportKind,
    pub batch: bool,
    /// The text sent: one envelope or the batch array.
    pub sent: String,
    /// The text received (the HTTP body, or the matching frame).
    pub received: Option<String>,
    pub calls: Vec<CallResult>,
    /// The last HTTP exchange: status, headers and timings.
    pub http: Option<Exchange>,
    pub attempts: Vec<Attempt>,
    pub subscription: Option<SubscriptionRecord>,
    /// Why the exchange produced no usable answer; redacted.
    pub error: Option<JsonrpcError>,
    pub elapsed: Duration,
    /// Over WebSocket: the trace context of the connection.s handshake,
    /// which every call on it shares, and its `traceresponse` header.
    pub trace: Option<SentTrace>,
    pub traceresponse: Option<String>,
}

impl RpcExchange {
    pub(crate) fn new(transport: TransportKind, batch: bool) -> Self {
        Self {
            transport,
            batch,
            sent: String::new(),
            received: None,
            calls: Vec::new(),
            http: None,
            attempts: Vec::new(),
            subscription: None,
            error: None,
            elapsed: Duration::ZERO,
            trace: None,
            traceresponse: None,
        }
    }

    /// Whether the batch was rejected as a whole (shown once).
    pub fn batch_rejected(&self) -> bool {
        self.calls.iter().any(|c| c.batch_rejected)
    }

    /// The failures of the calls, in call order; a rejected batch is
    /// reported once with [`BATCH_REJECTED`].
    pub fn failures(&self) -> Vec<String> {
        if self.batch_rejected() {
            let first = self.calls.iter().find_map(CallResult::failure);
            return first
                .map(|f| format!("{f} ({BATCH_REJECTED})"))
                .into_iter()
                .collect();
        }
        self.calls.iter().filter_map(CallResult::failure).collect()
    }

    /// Whether the request passed: no exchange error and every
    /// non-notification call returned a result.
    pub fn passed(&self) -> bool {
        self.error.is_none() && self.calls.iter().all(|c| c.failure().is_none())
    }

    /// The document assertions on call `index` read: its envelope, plus
    /// `events` for a subscription.
    pub fn envelope(&self, index: usize) -> Value {
        let mut envelope = self
            .calls
            .get(index)
            .map(CallResult::envelope)
            .unwrap_or_else(|| Value::Object(Map::new()));
        if let (Some(subscription), Value::Object(map)) = (&self.subscription, &mut envelope) {
            map.insert("events".into(), Value::Array(subscription.events.clone()));
        }
        envelope
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{IdSequence, request_body};
    use rustman_core::vars::ResolvedCall;
    use serde_json::json;

    fn two_calls() -> Vec<Envelope> {
        let calls = [
            ResolvedCall::new("eth_chainId", None),
            ResolvedCall::new("eth_blockNumber", None),
        ];
        request_body(&calls, true, &mut IdSequence::default()).0
    }

    #[test]
    fn out_of_order_batch_responses_match_by_id() {
        let response = json!([
            {"jsonrpc":"2.0","id":2,"result":"0x10"},
            {"jsonrpc":"2.0","id":1,"result":"0x1"},
        ]);
        let results = batch_results(&two_calls(), &response).unwrap();
        assert_eq!(results[0].outcome, CallOutcome::Result(json!("0x1")));
        assert_eq!(results[1].outcome, CallOutcome::Result(json!("0x10")));
    }

    #[test]
    fn a_missing_batch_item_fails_its_call() {
        let response = json!([{"jsonrpc":"2.0","id":1,"result":"0x1"}]);
        let results = batch_results(&two_calls(), &response).unwrap();
        assert!(results[0].failure().is_none());
        assert_eq!(
            results[1].failure().as_deref(),
            Some("no response for id 2")
        );
    }

    #[test]
    fn a_rejected_batch_fails_every_call_with_one_note() {
        let response =
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"batches disabled"}});
        let results = batch_results(&two_calls(), &response).unwrap();
        assert!(results.iter().all(|r| r.batch_rejected));
        let mut exchange = RpcExchange::new(TransportKind::Http, true);
        exchange.calls = results;
        assert_eq!(
            exchange.failures(),
            ["rpc error -32600: batches disabled (server rejected the batch)"]
        );
        assert!(!exchange.passed());
    }

    #[test]
    fn single_results_check_the_id() {
        let envelope = &two_calls()[0];
        let err = single_result(envelope, &json!({"jsonrpc":"2.0","id":2,"result":1})).unwrap_err();
        assert_eq!(err.to_string(), "response id 2 does not match request id 1");
        let ok = single_result(envelope, &json!({"jsonrpc":"2.0","id":1,"result":"0x10"})).unwrap();
        assert_eq!(ok.envelope()["result"], "0x10", "hex is kept as received");
    }
}
