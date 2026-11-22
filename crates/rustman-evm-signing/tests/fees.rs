//! Fee suggestion, the gas limit and nonces (task 5.3) against recorded
//! node answers: `eth_feeHistory` for EIP-1559, `eth_gasPrice` for
//! legacy, the pending count once per signer and an in-run counter after.

use std::path::Path;

use alloy_primitives::{Address, U256};
use rustman_core::model::TxType;
use rustman_evm::recorded::RecordedCall;
use rustman_evm::{Denomination, Recorded};
use rustman_evm_signing::fees::{Fee, FeeInputs, Fees, fees, gas_limit, suggest};
use rustman_evm_signing::nonce::Nonces;
use serde_json::json;

const GWEI: u64 = 1_000_000_000;

fn gwei_tenths(tenths: u64) -> U256 {
    U256::from(tenths * GWEI / 10)
}

fn node() -> Recorded {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fee_history.json");
    Recorded::from_file("http://node.test", &path).unwrap()
}

fn suggested(wei: U256) -> Fee {
    Fee {
        wei,
        suggested: true,
    }
}

#[tokio::test]
async fn eip1559_fees_come_from_the_fee_history() {
    // Scenario "Default fee suggestion": base fee 7.5 gwei, rewards with a
    // median of 1.5 gwei: max fee 2 × 7.5 + 1.5 = 16.5 gwei.
    let node = node();
    let fees = fees(&node, &FeeInputs::default(), &Denomination::default())
        .await
        .unwrap();
    assert_eq!(
        fees,
        Fees::Eip1559 {
            max_fee: suggested(gwei_tenths(165)),
            max_priority_fee: suggested(gwei_tenths(15)),
        }
    );
    assert_eq!(node.methods(), ["eth_feeHistory"]);
    let Fees::Eip1559 { max_fee, .. } = fees else {
        unreachable!()
    };
    assert_eq!(max_fee.show(), "16.5 gwei (suggested)");
}

#[tokio::test]
async fn fees_set_in_the_request_win_and_are_not_suggested() {
    let history = node();
    let inputs = FeeInputs {
        max_priority_fee: Some("2 gwei"),
        ..FeeInputs::default()
    };
    let partial = fees(&history, &inputs, &Denomination::default())
        .await
        .unwrap();
    let Fees::Eip1559 {
        max_fee,
        max_priority_fee,
    } = partial
    else {
        panic!("{partial:?}")
    };
    assert_eq!(max_priority_fee.wei, gwei_tenths(20));
    assert!(!max_priority_fee.suggested);
    assert_eq!(max_fee, suggested(gwei_tenths(170)));

    let both = FeeInputs {
        max_fee: Some("30 gwei"),
        max_priority_fee: Some("0x3b9aca00"),
        ..FeeInputs::default()
    };
    let quiet = node();
    let set = fees(&quiet, &both, &Denomination::default()).await.unwrap();
    assert!(quiet.methods().is_empty(), "nothing to suggest: no call");
    assert_eq!(set.max_per_gas(), gwei_tenths(300));
}

#[tokio::test]
async fn legacy_transactions_use_the_gas_price() {
    let node = node();
    let inputs = FeeInputs {
        tx_type: TxType::Legacy,
        ..FeeInputs::default()
    };
    let fees = fees(&node, &inputs, &Denomination::default())
        .await
        .unwrap();
    assert_eq!(
        fees,
        Fees::Legacy {
            gas_price: suggested(gwei_tenths(200))
        }
    );
    assert_eq!(node.methods(), ["eth_gasPrice"]);
}

#[tokio::test]
async fn fee_fields_of_the_other_type_are_refused() {
    let node = node();
    let legacy_with_max_fee = FeeInputs {
        tx_type: TxType::Legacy,
        max_fee: Some("1 gwei"),
        ..FeeInputs::default()
    };
    let err = fees(&node, &legacy_with_max_fee, &Denomination::default())
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 2);
    let eip1559_with_gas_price = FeeInputs {
        gas_price: Some("1 gwei"),
        ..FeeInputs::default()
    };
    let err = fees(&node, &eip1559_with_gas_price, &Denomination::default())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "gas_price is for type: legacy; an EIP-1559 transaction takes max_fee and max_priority_fee"
    );
    let too_much_priority = FeeInputs {
        max_fee: Some("1 gwei"),
        max_priority_fee: Some("2 gwei"),
        ..FeeInputs::default()
    };
    let err = fees(&node, &too_much_priority, &Denomination::default())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "max_priority_fee is above max_fee; lower it or raise max_fee"
    );
    assert!(node.methods().is_empty());
}

#[test]
fn a_fee_history_without_base_fees_is_not_eip1559() {
    let err = suggest(&json!({"oldestBlock": "0x1", "reward": []})).unwrap_err();
    assert_eq!(err.exit_code(), 3);
    assert!(err.to_string().contains("use type: legacy"), "{err}");
}

#[test]
fn the_gas_limit_adds_a_fifth_to_the_estimate() {
    let native = Denomination::default();
    assert_eq!(
        gas_limit(None, Some(U256::from(50_000)), &native).unwrap(),
        U256::from(60_000)
    );
}

#[tokio::test]
async fn nonces_come_from_the_pending_count_once_then_count_up() {
    // Scenario "Sequential transactions in a run".
    let signer: Address = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
        .parse()
        .unwrap();
    let calls: Vec<RecordedCall> = serde_json::from_value(json!([
        {"method": "eth_getTransactionCount",
         "params": ["0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266", "pending"],
         "result": "0x7"}
    ]))
    .unwrap();
    let node = Recorded::new("http://node.test", calls);
    let nonces = Nonces::new();
    let first = nonces.next(&node, 31_337, signer, None).await.unwrap();
    assert_eq!(first, 7);
    nonces.sent(31_337, signer, first);
    let second = nonces.next(&node, 31_337, signer, None).await.unwrap();
    assert_eq!(second, 8);
    assert_eq!(node.methods(), ["eth_getTransactionCount"], "asked once");
    // An explicit nonce wins and is not asked for.
    assert_eq!(
        nonces
            .next(&node, 31_337, signer, Some("42"))
            .await
            .unwrap(),
        42
    );
    // Another chain has its own count.
    let other = nonces.next(&node, 1, signer, None).await.unwrap();
    assert_eq!(other, 7);
    assert_eq!(node.methods().len(), 2);
}
