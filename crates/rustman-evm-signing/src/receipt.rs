//! What a mined transaction produced (evm-signing "Receipt wait and
//! audit"): the receipt decoded, the document receipt captures and `json`
//! assertions read, the `evm` assertions evaluated on it, and the audit
//! record kept in history.
//!
//! The audit record names the chain, the signer, the hash, the decoded
//! call, the fees and the outcome. It never holds the signed bytes, a
//! signature or any key material: none of them reach this module.

use std::collections::HashMap;
use std::sync::Arc;

use alloy_json_abi::JsonAbi;
use alloy_primitives::{B256, U256};
use rustman_core::assertions::AssertionResult;
use rustman_core::model::Assertion;
use rustman_evm::EvmError;
use rustman_evm::assert::{EvmFacts, evaluate_all};
use rustman_evm::codec::{DecodedLog, decode_log};
use rustman_evm::events::events_json;
use rustman_evm::quantity::Denomination;
use rustman_evm::rpc::Rpc;
use rustman_evm::tx::{DecodeContext, TokenCache, TxReport, fetch_tx};
use serde_json::{Value, json};

use crate::confirm::Summary;
use crate::fees::Fees;
use crate::send::explorer_url;

/// A mined transaction, decoded.
#[derive(Clone, Debug, PartialEq)]
pub struct Mined {
    pub report: TxReport,
    /// The receipt's logs decoded with the request's ABI.
    pub events: Vec<DecodedLog>,
    pub contract_address: Option<String>,
}

impl Mined {
    pub fn succeeded(&self) -> bool {
        self.report.status == Some(true)
    }
}

/// Fetch and decode the transaction `hash` and its receipt, its logs with
/// `abi` (the request's) where they come from its target.
pub async fn decode(
    rpc: &dyn Rpc,
    summary: &Summary,
    hash: B256,
    abi: &JsonAbi,
    receipt: &Value,
) -> Result<Mined, EvmError> {
    let mut abis = HashMap::new();
    abis.insert(summary.tx.to, Arc::new(abi.clone()));
    let tokens = TokenCache::new();
    let ctx = DecodeContext {
        rpc,
        chain: summary.chain.clone(),
        abis: &abis,
        tokens: &tokens,
    };
    let report = fetch_tx(&ctx, &hash.to_string()).await?;
    let events = report
        .logs
        .iter()
        .filter(|log| !log.removed)
        .filter_map(|log| decode_log(abi, &log.topics, &log.data))
        .collect();
    let contract_address = receipt["contractAddress"].as_str().map(str::to_owned);
    Ok(Mined {
        report,
        events,
        contract_address,
    })
}

fn status_text(status: Option<bool>) -> &'static str {
    match status {
        Some(true) => "success",
        Some(false) => "reverted",
        None => "pending",
    }
}

/// What receipt captures and `json` assertions read: `{"outputs": [],
/// "events": [...], "receipt": {contractAddress, blockNumber, status,
/// gasUsed}}`, quantities as decimal strings.
pub fn document(mined: &Mined) -> Value {
    let report = &mined.report;
    json!({
        "outputs": [],
        "events": events_json(&mined.events),
        "receipt": {
            "contractAddress": mined.contract_address,
            "blockNumber": report.block.map(|b| b.to_string()),
            "status": status_text(report.status),
            "gasUsed": report.gas_used.map(|g| g.to_string()),
        },
    })
}

/// The `evm` assertions of `assertions` on the receipt: `emits` reads its
/// events and `gas_lt` its gas used.
pub fn evm_assertions(
    assertions: &[Assertion],
    mined: &Mined,
    native: &Denomination,
) -> Vec<AssertionResult> {
    let facts = EvmFacts {
        outputs: None,
        revert: None,
        events: &mined.events,
        gas: mined.report.gas_used,
        native,
        token: None,
    };
    evaluate_all(assertions, &facts)
}

/// The audit record of a sent transaction, `mined` once its receipt came.
pub fn audit(summary: &Summary, hash: B256, mined: Option<&Mined>) -> Value {
    let tx = &summary.tx;
    let quantity = |v: U256| v.to_string();
    let fees = match tx.fees {
        Fees::Eip1559 {
            max_fee,
            max_priority_fee,
        } => json!({
            "type": "eip1559",
            "max_fee": quantity(max_fee.wei),
            "max_priority_fee": quantity(max_priority_fee.wei),
            "suggested": max_fee.suggested || max_priority_fee.suggested,
        }),
        Fees::Legacy { gas_price } => json!({
            "type": "legacy",
            "gas_price": quantity(gas_price.wei),
            "suggested": gas_price.suggested,
        }),
    };
    let report = mined.map(|m| &m.report);
    json!({
        "kind": "evm-tx",
        "chain_id": summary.chain.id,
        "chain": summary.chain.name,
        "signer": {"id": summary.signer_id, "address": summary.signer.to_checksum(None)},
        "hash": hash.to_string(),
        "to": tx.to.to_checksum(None),
        "to_name": summary.to_name,
        "call": summary.call,
        "value": quantity(tx.value),
        "nonce": tx.nonce,
        "gas_limit": quantity(tx.gas_limit),
        "fees": fees,
        "forced_revert": summary.reverted.is_some(),
        "status": status_text(report.and_then(|r| r.status)),
        "block": report.and_then(|r| r.block),
        "gas_used": report.and_then(|r| r.gas_used).map(quantity),
        "effective_gas_price": report.and_then(|r| r.effective_gas_price).map(quantity),
        "fee": report.and_then(TxReport::fee).map(quantity),
        "explorer": explorer_url(&summary.chain, hash),
    })
}

/// The status of a sent transaction whose receipt could not be read (the
/// node kept failing, or the receipt could not be decoded).
pub const RECEIPT_UNKNOWN: &str = "receipt-unknown";

/// The audit record of a sent transaction whose receipt is unknown: the
/// hash, chain, signer, `to`, nonce and fees, with `status`
/// `receipt-unknown`.
pub fn audit_receipt_unknown(summary: &Summary, hash: B256) -> Value {
    let mut record = audit(summary, hash, None);
    record["status"] = json!(RECEIPT_UNKNOWN);
    record
}
