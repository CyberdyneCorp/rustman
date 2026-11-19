//! An [`RpcExchange`] as the runner checks and records it: one
//! [`JsonrpcCallCheck`] per call (the envelope assertions and captures
//! read, design D3) and a [`JsonrpcRecord`] for history and reports.
//! Redaction happens in the runner, which knows every secret of the run.

use rustman_core::run::{JsonrpcRecord, RpcAttemptRecord, RpcCallRecord, RpcSubscriptionRecord};
use rustman_http::millis;
use rustman_http::runner::JsonrpcCallCheck;

use crate::outcome::{
    Attempt, BATCH_REJECTED, CallOutcome, CallResult, RpcExchange, TransportKind,
};
use crate::subscription::SubscriptionRecord;

/// What the assertions and captures of each call read.
pub fn call_checks(exchange: &RpcExchange) -> Vec<JsonrpcCallCheck> {
    (0..exchange.calls.len())
        .map(|index| {
            let call = &exchange.calls[index];
            JsonrpcCallCheck {
                envelope: exchange.envelope(index),
                failure: call.failure(),
                rpc_error: matches!(call.outcome, CallOutcome::Error(_)),
            }
        })
        .collect()
}

/// The history record of `exchange` (not yet redacted).
pub fn exchange_record(exchange: &RpcExchange) -> JsonrpcRecord {
    JsonrpcRecord {
        transport: match exchange.transport {
            TransportKind::Http => "http",
            TransportKind::WebSocket => "ws",
        }
        .to_owned(),
        batch: exchange.batch,
        calls: exchange.calls.iter().map(call_record).collect(),
        attempts: exchange.attempts.iter().map(attempt_record).collect(),
        subscription: exchange.subscription.as_ref().map(subscription_record),
        note: exchange.batch_rejected().then(|| BATCH_REJECTED.to_owned()),
        refusal: None,
        elapsed_ms: millis(exchange.elapsed),
    }
}

fn call_record(call: &CallResult) -> RpcCallRecord {
    let outcome = match call.outcome {
        CallOutcome::Result(_) => "result",
        CallOutcome::Error(_) => "error",
        CallOutcome::Notification => "notification",
        CallOutcome::Failed(_) => "failed",
    };
    RpcCallRecord {
        method: call.method.clone(),
        id: call.id.clone(),
        request: call.request.clone(),
        response: call.response.clone(),
        outcome: outcome.to_owned(),
        failure: call.failure(),
    }
}

fn attempt_record(attempt: &Attempt) -> RpcAttemptRecord {
    RpcAttemptRecord {
        status: attempt.status,
        error: attempt.error.clone(),
        elapsed_ms: millis(attempt.elapsed),
        retry_in_ms: attempt.retry_in.map(millis),
    }
}

fn subscription_record(subscription: &SubscriptionRecord) -> RpcSubscriptionRecord {
    RpcSubscriptionRecord {
        ended: subscription.ended.as_str().to_owned(),
        unsubscribed: subscription.unsubscribed,
        elapsed_ms: millis(subscription.elapsed),
        ..RpcSubscriptionRecord::new(
            subscription.subscription_id.clone(),
            subscription.event_count,
            subscription.events.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{IdSequence, request_body};
    use crate::outcome::batch_results;
    use rustman_core::vars::ResolvedCall;
    use serde_json::json;

    #[test]
    fn batch_calls_are_checked_against_their_own_envelope() {
        let calls = [ResolvedCall::new("a", None), ResolvedCall::new("b", None)];
        let envelopes = request_body(&calls, true, &mut IdSequence::default()).0;
        let response = json!([
            {"jsonrpc":"2.0","id":2,"error":{"code":-32602,"message":"bad"}},
            {"jsonrpc":"2.0","id":1,"result":"0x1"},
        ]);
        let mut exchange = RpcExchange::new(TransportKind::Http, true);
        exchange.calls = batch_results(&envelopes, &response).unwrap();
        let checks = call_checks(&exchange);
        assert_eq!(checks[0].envelope["result"], "0x1");
        assert!(checks[0].failure.is_none());
        assert_eq!(checks[1].envelope["error"]["code"], -32602);
        assert!(checks[1].rpc_error);
        let record = exchange_record(&exchange);
        assert_eq!(record.transport, "http");
        assert_eq!(record.calls[1].outcome, "error");
        assert_eq!(
            record.calls[1].failure.as_deref(),
            Some("rpc error -32602: bad")
        );
    }
}
