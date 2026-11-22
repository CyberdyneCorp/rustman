//! Signing, sending and waiting for the receipt (evm-signing "Receipt
//! wait and audit", design D2).
//!
//! [`sign_and_send`] is the only place a transaction is signed. It needs a
//! [`Confirmation`] of exactly that transaction, then sends the signed
//! bytes with `eth_sendRawTransaction`. That method is refused by the
//! send-method guard for every other path; [`Broadcast`] for
//! [`JsonrpcTransport`] sends it through `rustman-jsonrpc` without asking
//! the guards, which only this crate can do. The send is never retried:
//! an RPC error such as `nonce too low` is reported verbatim. A failed
//! send may still have reached the network, so it ends as
//! [`Broadcasted::Unknown`] with the hash computed from the signed bytes,
//! and the receipt is still awaited for that hash. The receipt
//! polls are (the transaction is out by then): a failing poll is tried
//! again until the receipt timeout.

use std::time::Duration;

use alloy_primitives::B256;
use rustman_core::vars::{ResolvedCall, ResolvedJsonrpc};
use rustman_evm::quantity::parse_hex_u64;
use rustman_evm::rpc::{JsonrpcTransport, Rpc, RpcFailure, RpcFuture, call};
use rustman_evm::{ChainInfo, EvmError};
use rustman_jsonrpc::JsonrpcClient;
use rustman_jsonrpc::outcome::CallOutcome;
use serde_json::{Value, json};

use crate::confirm::Confirmation;
use crate::signer::UnlockedSigner;
use crate::transaction::UnsignedTx;

/// The method signed transactions are sent with.
pub const SEND_RAW: &str = "eth_sendRawTransaction";

/// The default receipt timeout.
pub const RECEIPT_TIMEOUT: Duration = Duration::from_secs(120);

/// Sends signed transactions. Only this crate implements it for a real
/// node.
pub trait Broadcast {
    /// Send `raw` (`0x` hex) with `eth_sendRawTransaction`.
    fn send_raw<'a>(&'a self, raw: String) -> RpcFuture<'a>;
}

impl Broadcast for JsonrpcTransport {
    fn send_raw<'a>(&'a self, raw: String) -> RpcFuture<'a> {
        Box::pin(async move {
            let call = ResolvedCall::new(SEND_RAW, Some(json!([raw])));
            let target = ResolvedJsonrpc::new(self.request().clone(), vec![call], false);
            let client = JsonrpcClient::new(self.sender());
            let exchange = client.send(&target, self.options()).await;
            let redactor = self.redactor();
            if let Some(error) = &exchange.error {
                return Err(RpcFailure::Transport(
                    redactor.text(&error.to_string()).into_owned(),
                ));
            }
            match exchange.calls.into_iter().next().map(|c| c.outcome) {
                Some(CallOutcome::Result(value)) => Ok(value),
                Some(CallOutcome::Error(error)) => Err(RpcFailure::Error {
                    code: error.code,
                    message: redactor.text(&error.message).into_owned(),
                    data: None,
                }),
                Some(CallOutcome::Failed(reason)) => {
                    Err(RpcFailure::Transport(redactor.text(&reason).into_owned()))
                }
                Some(CallOutcome::Notification) | None => {
                    Err(RpcFailure::Transport("no response".to_owned()))
                }
            }
        })
    }
}

/// What the node made of a signed transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Broadcasted {
    /// The node answered the transaction's hash.
    Accepted(B256),
    /// The send failed (transport error, RPC error, or an answer that is
    /// not the transaction's hash): the transaction may or may not be out.
    /// `hash` is computed locally from the signed bytes; `reason` is the
    /// failure, already redacted.
    Unknown { hash: B256, reason: String },
}

impl Broadcasted {
    /// The transaction hash, computed locally from the signed bytes.
    pub fn hash(&self) -> B256 {
        match self {
            Self::Accepted(hash) | Self::Unknown { hash, .. } => *hash,
        }
    }
}

/// Sign `tx` with `signer` and send it. The confirmation must be of this
/// very transaction; a mismatch is a refusal and nothing is signed. Once
/// signed, the outcome is [`Broadcasted`]: a failed send is not an error,
/// since the transaction may have been broadcast all the same.
pub async fn sign_and_send(
    tx: &UnsignedTx,
    signer: &UnlockedSigner,
    confirmation: Confirmation,
    node: &dyn Broadcast,
) -> Result<Broadcasted, EvmError> {
    if !confirmation.covers(tx) {
        return Err(EvmError::Refused(
            "the confirmation is for another transaction; nothing was signed or sent".to_owned(),
        ));
    }
    let signed = tx.sign(signer);
    let hash = signed.hash;
    let reason = match node.send_raw(signed.raw_hex()).await {
        Err(failure) => failure.into_error(SEND_RAW).to_string(),
        Ok(answer) => match answer.as_str().and_then(|text| text.parse::<B256>().ok()) {
            Some(answered) if answered == hash => return Ok(Broadcasted::Accepted(hash)),
            Some(answered) => {
                format!("{SEND_RAW}: the node answered hash {answered}, expected {hash}")
            }
            None => format!("{SEND_RAW}: the node answered no transaction hash"),
        },
    };
    Ok(Broadcasted::Unknown { hash, reason })
}

/// How to wait for a receipt.
#[derive(Clone, Copy, Debug)]
pub struct Wait {
    /// Blocks on top of the receipt's own, counting it (1: mined).
    pub confirmations: u64,
    pub timeout: Duration,
    /// Time between two polls.
    pub poll: Duration,
}

impl Default for Wait {
    fn default() -> Self {
        Self {
            confirmations: 1,
            timeout: RECEIPT_TIMEOUT,
            poll: Duration::from_millis(500),
        }
    }
}

/// How the wait ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Waited {
    /// The receipt, with enough confirmations.
    Mined(Value),
    /// Not mined (or not confirmed) within the timeout.
    TimedOut,
    /// The node kept failing until the timeout: whether the transaction
    /// was mined is unknown. The last error, already redacted.
    Unknown(String),
}

/// The longest pause between two polls after errors.
const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// Poll `eth_getTransactionReceipt` for `hash` until it has
/// `wait.confirmations` blocks or `wait.timeout` passes. The transaction
/// is already sent, so an error does not end the wait: the poll is tried
/// again, backing off (doubling up to 5 s) while errors repeat, until the
/// timeout. It ends as [`Waited::Unknown`] when the last poll failed.
pub async fn wait_for_receipt(rpc: &dyn Rpc, hash: B256, wait: &Wait) -> Waited {
    let deadline = tokio::time::Instant::now() + wait.timeout;
    let mut failures: u32 = 0;
    loop {
        let last_error = match confirmed_receipt(rpc, hash, wait.confirmations).await {
            Ok(Some(receipt)) => return Waited::Mined(receipt),
            Ok(None) => {
                failures = 0;
                None
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                Some(error.to_string())
            }
        };
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return match last_error {
                Some(error) => Waited::Unknown(error),
                None => Waited::TimedOut,
            };
        }
        tokio::time::sleep(backoff(wait.poll, failures).min(deadline - now)).await;
    }
}

/// The pause before the next poll: `poll`, doubled for each failure in a
/// row, at most [`MAX_BACKOFF`] (never below `poll`).
fn backoff(poll: Duration, failures: u32) -> Duration {
    let factor = 1_u32 << failures.min(10);
    poll.saturating_mul(factor).min(MAX_BACKOFF.max(poll))
}

async fn confirmed_receipt(
    rpc: &dyn Rpc,
    hash: B256,
    confirmations: u64,
) -> Result<Option<Value>, EvmError> {
    let receipt = call(rpc, "eth_getTransactionReceipt", json!([hash.to_string()])).await?;
    let Some(mined) = receipt["blockNumber"]
        .as_str()
        .and_then(|b| parse_hex_u64(b).ok())
    else {
        return Ok(None);
    };
    if confirmations <= 1 {
        return Ok(Some(receipt));
    }
    let head = call(rpc, "eth_blockNumber", json!([])).await?;
    let head = head
        .as_str()
        .and_then(|h| parse_hex_u64(h).ok())
        .ok_or_else(|| EvmError::Rpc("eth_blockNumber: not a quantity".to_owned()))?;
    let depth = head.saturating_sub(mined).saturating_add(1);
    Ok((depth >= confirmations).then_some(receipt))
}

/// The explorer page of `hash` on `chain`, when the chain has an explorer.
pub fn explorer_url(chain: &ChainInfo, hash: B256) -> Option<String> {
    chain
        .explorer
        .as_ref()
        .map(|base| format!("{}/tx/{hash}", base.trim_end_matches('/')))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use rustman_evm::ChainRegistry;
    use rustman_evm::rpc::RpcFailure;

    use super::*;

    /// A node whose receipt polls fail `failures` times, then answer
    /// `receipt` (or `null` when `None`).
    struct Flaky {
        failures: Mutex<u32>,
        receipt: Option<Value>,
        polls: Mutex<u32>,
    }

    impl Rpc for Flaky {
        fn endpoint(&self) -> String {
            "http://flaky.test".to_owned()
        }

        fn call<'a>(&'a self, method: &'a str, _params: Value) -> RpcFuture<'a> {
            Box::pin(async move {
                assert_eq!(method, "eth_getTransactionReceipt");
                *self.polls.lock().unwrap() += 1;
                let mut failures = self.failures.lock().unwrap();
                if *failures > 0 {
                    *failures -= 1;
                    return Err(RpcFailure::Transport("connection reset".to_owned()));
                }
                Ok(self.receipt.clone().unwrap_or(Value::Null))
            })
        }
    }

    fn flaky(failures: u32, receipt: Option<Value>) -> Flaky {
        Flaky {
            failures: Mutex::new(failures),
            receipt,
            polls: Mutex::new(0),
        }
    }

    fn quick(timeout_ms: u64) -> Wait {
        Wait {
            confirmations: 1,
            timeout: Duration::from_millis(timeout_ms),
            poll: Duration::from_millis(5),
        }
    }

    #[tokio::test]
    async fn receipt_polls_that_fail_are_tried_again() {
        // Regression: the first failing poll ended the wait with an error,
        // so a transaction that was sent got no audit record.
        let receipt = json!({"blockNumber": "0x1", "status": "0x1"});
        let node = flaky(3, Some(receipt.clone()));
        let waited = wait_for_receipt(&node, B256::ZERO, &quick(5_000)).await;
        assert_eq!(waited, Waited::Mined(receipt));
        assert_eq!(*node.polls.lock().unwrap(), 4);
    }

    #[tokio::test]
    async fn a_node_that_keeps_failing_leaves_the_receipt_unknown() {
        let node = flaky(u32::MAX, None);
        let waited = wait_for_receipt(&node, B256::ZERO, &quick(100)).await;
        let Waited::Unknown(error) = waited else {
            panic!("{waited:?}");
        };
        assert!(error.contains("connection reset"), "{error}");
        let node = flaky(0, None);
        let waited = wait_for_receipt(&node, B256::ZERO, &quick(50)).await;
        assert_eq!(waited, Waited::TimedOut);
    }

    #[test]
    fn the_backoff_doubles_up_to_five_seconds() {
        let poll = Duration::from_millis(250);
        assert_eq!(backoff(poll, 0), poll);
        assert_eq!(backoff(poll, 2), Duration::from_secs(1));
        assert_eq!(backoff(poll, 40), MAX_BACKOFF);
        assert_eq!(backoff(Duration::from_secs(9), 3), Duration::from_secs(9));
    }

    #[test]
    fn explorer_links() {
        let hash = B256::repeat_byte(0xab);
        let sepolia = ChainRegistry::built_in().get(11_155_111);
        assert_eq!(
            explorer_url(&sepolia, hash).unwrap(),
            format!("https://sepolia.etherscan.io/tx/{hash}")
        );
        let local = ChainRegistry::built_in().get(31_337);
        assert_eq!(explorer_url(&local, hash), None);
    }
}
