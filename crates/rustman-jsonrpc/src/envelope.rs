//! JSON-RPC 2.0 envelopes (design D1): building requests with their ids,
//! and classifying responses. Members are written in the order
//! `jsonrpc`, `method`, `params`, `id`.

use std::fmt;

use rustman_core::vars::ResolvedCall;
use serde_json::{Map, Value};

use crate::error::JsonrpcError;

/// Sequential integer ids, starting at 1, for one connection.
#[derive(Clone, Debug)]
pub struct IdSequence {
    next: u64,
}

impl Default for IdSequence {
    fn default() -> Self {
        Self { next: 1 }
    }
}

impl IdSequence {
    /// The next id.
    pub fn take(&mut self) -> Value {
        let id = self.next;
        self.next += 1;
        Value::from(id)
    }

    /// The next id that is not one of `taken` (the explicit ids of the
    /// same request), so no two calls of a batch share an id.
    pub fn take_avoiding(&mut self, taken: &[Value]) -> Value {
        loop {
            let id = self.take();
            if !taken.contains(&id) {
                return id;
            }
        }
    }
}

/// One request envelope and the id it carries (`None` for a notification).
#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    pub value: Value,
    pub id: Option<Value>,
}

impl Envelope {
    /// The envelope of `call`: an explicit id is sent unchanged, a missing
    /// one comes from `ids`, and notifications carry none.
    pub fn of(call: &ResolvedCall, ids: &mut IdSequence) -> Self {
        Self::avoiding(call, ids, &[])
    }

    /// [`Envelope::of`], where a missing id is none of `taken`.
    fn avoiding(call: &ResolvedCall, ids: &mut IdSequence, taken: &[Value]) -> Self {
        let id = (!call.notification)
            .then(|| call.id.clone().unwrap_or_else(|| ids.take_avoiding(taken)));
        let mut value = Map::new();
        value.insert("jsonrpc".into(), Value::from("2.0"));
        value.insert("method".into(), Value::from(call.method.clone()));
        if let Some(params) = &call.params {
            value.insert("params".into(), params.clone());
        }
        if let Some(id) = &id {
            value.insert("id".into(), id.clone());
        }
        Self {
            value: Value::Object(value),
            id,
        }
    }
}

/// The envelopes of `calls`, and the body that carries them: one object,
/// or an array for a batch.
pub fn request_body(
    calls: &[ResolvedCall],
    batch: bool,
    ids: &mut IdSequence,
) -> (Vec<Envelope>, Value) {
    let taken: Vec<Value> = calls
        .iter()
        .filter(|c| !c.notification)
        .filter_map(|c| c.id.clone())
        .collect();
    let envelopes: Vec<Envelope> = calls
        .iter()
        .map(|c| Envelope::avoiding(c, ids, &taken))
        .collect();
    let body = match envelopes.as_slice() {
        [single] if !batch => single.value.clone(),
        many => Value::Array(many.iter().map(|e| e.value.clone()).collect()),
    };
    (envelopes, body)
}

/// A JSON-RPC error object.
#[derive(Clone, Debug, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rpc error {}: {}", self.code, self.message)
    }
}

/// What a well-formed response carries.
#[derive(Clone, Debug, PartialEq)]
pub enum Response {
    Result(Value),
    Error(RpcError),
}

/// Read `value` as a JSON-RPC 2.0 response; the error names the reason
/// (`invalid JSON-RPC response: <reason>`).
pub fn classify(value: &Value) -> Result<Response, JsonrpcError> {
    let invalid = |reason: &str| Err(JsonrpcError::Invalid(reason.to_owned()));
    let Some(object) = value.as_object() else {
        return invalid("not a JSON object");
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return invalid("jsonrpc must be \"2.0\"");
    }
    match (object.get("result"), object.get("error")) {
        (Some(_), Some(_)) => invalid("both result and error present"),
        (None, None) => invalid("neither result nor error present"),
        (Some(result), None) => Ok(Response::Result(result.clone())),
        (None, Some(error)) => error_object(error).map(Response::Error),
    }
}

fn error_object(error: &Value) -> Result<RpcError, JsonrpcError> {
    let code = error.get("code").and_then(Value::as_i64);
    let message = error.get("message").and_then(Value::as_str);
    match (code, message) {
        (Some(code), Some(message)) => Ok(RpcError {
            code,
            message: message.to_owned(),
            data: error.get("data").cloned(),
        }),
        _ => Err(JsonrpcError::Invalid(
            "error must have an integer code and a string message".to_owned(),
        )),
    }
}

/// Whether `value` looks like a JSON-RPC response object (has `result` or
/// `error`), as opposed to a request or notification from the server.
pub fn is_response(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.contains_key("result") || o.contains_key("error"))
}

/// The id of a response, as written (`null` when absent).
pub fn response_id(value: &Value) -> &Value {
    value.get("id").unwrap_or(&Value::Null)
}

/// An id as messages show it: integers bare, strings quoted.
pub fn show_id(id: &Value) -> String {
    id.to_string()
}

/// Check that `response` answers the request with id `expected`. A server
/// error about the request itself (id `null`, such as a parse error) is
/// accepted as the answer.
pub fn check_id(response: &Value, expected: &Value) -> Result<(), JsonrpcError> {
    let got = response_id(response);
    if got == expected || (got.is_null() && response.get("error").is_some()) {
        return Ok(());
    }
    Err(JsonrpcError::IdMismatch {
        got: show_id(got),
        expected: show_id(expected),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(method: &str) -> ResolvedCall {
        ResolvedCall::new(method, None)
    }

    #[test]
    fn a_minimal_call_has_sequential_id_one() {
        let mut ids = IdSequence::default();
        let (_, body) = request_body(&[call("eth_blockNumber")], false, &mut ids);
        assert_eq!(
            body.to_string(),
            r#"{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}"#
        );
    }

    #[test]
    fn positional_and_named_params() {
        let mut ids = IdSequence::default();
        let positional = ResolvedCall::new("getblockhash", Some(json!([800000])));
        let env = Envelope::of(&positional, &mut ids);
        assert!(env.value.to_string().contains(r#""params":[800000]"#));
        let named = ResolvedCall::new(
            "getBalance",
            Some(json!({"address": "Abc1", "commitment": "finalized"})),
        );
        let env = Envelope::of(&named, &mut ids);
        assert!(
            env.value
                .to_string()
                .contains(r#""params":{"address":"Abc1","commitment":"finalized"}"#)
        );
        assert_eq!(env.id, Some(json!(2)));
    }

    #[test]
    fn explicit_ids_are_sent_unchanged() {
        let mut ids = IdSequence::default();
        let mut c = call("m");
        c.id = Some(json!("req-7"));
        let env = Envelope::of(&c, &mut ids);
        assert!(env.value.to_string().contains(r#""id":"req-7""#));
        assert_eq!(ids.take(), json!(1), "explicit ids take no sequence number");
    }

    #[test]
    fn notifications_carry_no_id() {
        let mut ids = IdSequence::default();
        let mut c = call("log");
        c.notification = true;
        let env = Envelope::of(&c, &mut ids);
        assert_eq!(env.value, json!({"jsonrpc": "2.0", "method": "log"}));
        assert_eq!(env.id, None);
    }

    #[test]
    fn a_batch_is_an_array_with_ids_one_and_two() {
        let mut ids = IdSequence::default();
        let (envelopes, body) = request_body(
            &[call("eth_chainId"), call("eth_blockNumber")],
            true,
            &mut ids,
        );
        assert_eq!(envelopes.len(), 2);
        assert_eq!(body[0]["id"], 1);
        assert_eq!(body[1]["id"], 2);
        let (_, one) = request_body(&[call("a")], true, &mut IdSequence::default());
        assert!(one.is_array(), "a one-call batch is still an array");
    }

    #[test]
    fn auto_ids_skip_the_explicit_ids_of_the_batch() {
        let mut first = call("a");
        first.id = Some(json!(1));
        let mut third = call("c");
        third.id = Some(json!(2));
        let (envelopes, _) = request_body(
            &[first, call("b"), third, call("d")],
            true,
            &mut IdSequence::default(),
        );
        let ids: Vec<Value> = envelopes.into_iter().filter_map(|e| e.id).collect();
        assert_eq!(ids, vec![json!(1), json!(3), json!(2), json!(4)]);
    }

    #[test]
    fn results_and_errors_are_classified() {
        let ok = json!({"jsonrpc": "2.0", "id": 1, "result": "0x10"});
        assert_eq!(classify(&ok).unwrap(), Response::Result(json!("0x10")));
        let err =
            json!({"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}});
        let Response::Error(e) = classify(&err).unwrap() else {
            panic!("an error object");
        };
        assert_eq!(e.to_string(), "rpc error -32601: Method not found");
        assert_eq!(e.data, None);
    }

    #[test]
    fn malformed_responses_name_the_reason() {
        let cases = [
            (
                json!({"jsonrpc":"2.0","id":1,"result":1,"error":{"code":1,"message":"m"}}),
                "both result and error present",
            ),
            (
                json!({"jsonrpc":"2.0","id":1}),
                "neither result nor error present",
            ),
            (
                json!({"jsonrpc":"1.0","id":1,"result":1}),
                "jsonrpc must be \"2.0\"",
            ),
            (json!([1]), "not a JSON object"),
            (
                json!({"jsonrpc":"2.0","id":1,"error":{"code":"x"}}),
                "error must have an integer code and a string message",
            ),
        ];
        for (value, reason) in cases {
            let err = classify(&value).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("invalid JSON-RPC response: {reason}")
            );
        }
    }

    #[test]
    fn mismatched_ids_are_errors() {
        let response = json!({"jsonrpc":"2.0","id":2,"result":1});
        let err = check_id(&response, &json!(1)).unwrap_err();
        assert_eq!(err.to_string(), "response id 2 does not match request id 1");
        assert!(check_id(&response, &json!(2)).is_ok());
        let parse_error =
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}});
        assert!(check_id(&parse_error, &json!(1)).is_ok());
        let err = check_id(&json!({"id": "a", "result": 1}), &json!("b")).unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"response id "a" does not match request id "b""#
        );
    }
}
