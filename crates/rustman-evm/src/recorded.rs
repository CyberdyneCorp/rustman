//! [`Recorded`]: an [`Rpc`] replaying recorded node answers, for tests
//! only. It exists with the `test-fixtures` feature, which only test
//! builds turn on (the dev-dependency of this crate on itself), never a
//! shipped build.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use serde::Deserialize;
use serde_json::Value;

use crate::error::EvmError;
use crate::rpc::{METHOD_NOT_FOUND, Rpc, RpcFailure, RpcFuture};

/// One recorded answer: a `result` or an `error`, for `method` and, when
/// given, exactly these `params`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedCall {
    pub method: String,
    pub params: Option<Value>,
    pub result: Option<Value>,
    pub error: Option<RecordedError>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

/// Replays recorded answers and remembers every call made. A method with
/// no recording answers `-32601`, as a node without it would.
pub struct Recorded {
    endpoint: String,
    calls: Vec<RecordedCall>,
    seen: Mutex<Vec<(String, Value)>>,
}

impl Recorded {
    pub fn new(endpoint: &str, calls: Vec<RecordedCall>) -> Self {
        Self {
            endpoint: endpoint.to_owned(),
            calls,
            seen: Mutex::new(Vec::new()),
        }
    }

    /// The recordings of a JSON file: an array of [`RecordedCall`]s.
    pub fn from_file(endpoint: &str, path: &Path) -> Result<Self, EvmError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| EvmError::config(format!("{}: {e}", path.display())))?;
        Self::from_json(endpoint, &text)
    }

    pub fn from_json(endpoint: &str, text: &str) -> Result<Self, EvmError> {
        let calls = serde_json::from_str(text)
            .map_err(|e| EvmError::config(format!("recorded calls: {e}")))?;
        Ok(Self::new(endpoint, calls))
    }

    /// Every call made so far: method and params, in order.
    pub fn seen(&self) -> Vec<(String, Value)> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The methods called so far, in order.
    pub fn methods(&self) -> Vec<String> {
        self.seen().into_iter().map(|(m, _)| m).collect()
    }

    fn answer(&self, method: &str, params: &Value) -> Result<Value, RpcFailure> {
        let found = self
            .calls
            .iter()
            .filter(|c| c.method == method)
            .find(|c| c.params.as_ref().is_none_or(|p| p == params));
        let Some(recorded) = found else {
            return Err(RpcFailure::Error {
                code: METHOD_NOT_FOUND,
                message: format!("the method {method} does not exist/is not available"),
                data: None,
            });
        };
        match &recorded.error {
            Some(error) => Err(RpcFailure::Error {
                code: error.code,
                message: error.message.clone(),
                data: error.data.clone(),
            }),
            None => Ok(recorded.result.clone().unwrap_or(Value::Null)),
        }
    }
}

impl Rpc for Recorded {
    fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    fn call<'a>(&'a self, method: &'a str, params: Value) -> RpcFuture<'a> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((method.to_owned(), params.clone()));
        let answer = self.answer(method, &params);
        Box::pin(async move { answer })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::rpc::call;

    #[tokio::test]
    async fn recordings_answer_by_method_and_params() {
        let rpc = Recorded::from_json(
            "fixture",
            r#"[
              {"method": "eth_call", "params": [{"data": "0x01"}, "latest"], "result": "0xaa"},
              {"method": "eth_call", "result": "0xbb"},
              {"method": "eth_chainId", "error": {"code": -32000, "message": "boom"}}
            ]"#,
        )
        .unwrap();
        let first = rpc
            .call("eth_call", json!([{"data": "0x01"}, "latest"]))
            .await;
        assert_eq!(first, Ok(json!("0xaa")));
        let other = rpc
            .call("eth_call", json!([{"data": "0x02"}, "latest"]))
            .await;
        assert_eq!(other, Ok(json!("0xbb")));
        let err = call(&rpc, "eth_chainId", json!([])).await.unwrap_err();
        assert_eq!(
            err,
            EvmError::Rpc("eth_chainId: rpc error -32000: boom".into())
        );
        let missing = rpc.call("trace_call", json!([])).await.unwrap_err();
        assert!(missing.is_method_not_found());
        assert_eq!(
            rpc.methods(),
            ["eth_call", "eth_call", "eth_chainId", "trace_call"]
        );
    }

    #[test]
    fn refusals_exit_with_4() {
        let err = RpcFailure::Refused("no".into()).into_error("eth_sendRawTransaction");
        assert_eq!(err.exit_code(), 4);
        assert_eq!(
            RpcFailure::Transport("t".into())
                .into_error("m")
                .exit_code(),
            3
        );
    }
}
