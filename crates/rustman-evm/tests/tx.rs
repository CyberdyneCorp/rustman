//! Transaction and receipt decoding against a recorded mainnet USDC
//! transfer, and token metadata caching with the `decimals()` warning
//! (task 2.5).

use std::collections::HashMap;
use std::path::PathBuf;

use rustman_evm::tx::{DecodeContext, NO_DECIMALS, TX_NOT_FOUND, TokenCache, fetch_tx};
use rustman_evm::{ChainRegistry, Recorded};

const USDC_TX: &str = "0xdeed52ec1e92d0ca8834284f39ed5e37ff6106b6bb09a9dc37fb6d1c01b0a8f6";

fn recorded(name: &str) -> Recorded {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    Recorded::from_file("http://node.test/", &path).unwrap()
}

#[tokio::test]
async fn a_usdc_transfer_decodes_in_token_units() {
    let rpc = recorded("mainnet/usdc-transfer.json");
    let abis = HashMap::new();
    let tokens = TokenCache::new();
    let ctx = DecodeContext {
        rpc: &rpc,
        chain: ChainRegistry::built_in().get(1),
        abis: &abis,
        tokens: &tokens,
    };
    let report = fetch_tx(&ctx, USDC_TX).await.unwrap();
    assert_eq!(
        report.logs[0].decoded.as_deref(),
        Some(
            "Transfer(from=0x7e4c83cb3b38C128E42286dB909E410B1bC60038, to=0x5e3D81ECeb0c7D9bA1E6cfDAcE631B165a149d3F, value=250.00 USDC)"
        )
    );
    insta::assert_snapshot!("usdc_transfer_text", report.render());
    insta::assert_json_snapshot!("usdc_transfer_json", report.to_json());
    fetch_tx(&ctx, USDC_TX).await.unwrap();
    let token_calls = rpc.methods().iter().filter(|m| *m == "eth_call").count();
    assert_eq!(token_calls, 2, "decimals() and symbol() once per session");
}

#[tokio::test]
async fn a_token_without_decimals_shows_raw_amounts_with_a_warning() {
    let rpc = recorded("tx/no-decimals.json");
    let abis = HashMap::new();
    let tokens = TokenCache::new();
    let ctx = DecodeContext {
        rpc: &rpc,
        chain: ChainRegistry::built_in().get(1),
        abis: &abis,
        tokens: &tokens,
    };
    let hash = "0x1111111111111111111111111111111111111111111111111111111111111111";
    let report = fetch_tx(&ctx, hash).await.unwrap();
    assert_eq!(report.warnings, [NO_DECIMALS]);
    assert_eq!(NO_DECIMALS, "decimals() unavailable; showing raw amount");
    insta::assert_snapshot!("no_decimals_text", report.render());
    let token_calls = rpc.methods().iter().filter(|m| *m == "eth_call").count();
    assert_eq!(token_calls, 2, "two logs of one token ask it once");
}

#[tokio::test]
async fn an_unknown_transaction_is_not_found_with_exit_1() {
    let rpc = recorded("tx/no-decimals.json");
    let abis = HashMap::new();
    let tokens = TokenCache::new();
    let ctx = DecodeContext {
        rpc: &rpc,
        chain: ChainRegistry::built_in().get(1),
        abis: &abis,
        tokens: &tokens,
    };
    let hash = "0x2222222222222222222222222222222222222222222222222222222222222222";
    let err = fetch_tx(&ctx, hash).await.unwrap_err();
    assert_eq!(err.to_string(), TX_NOT_FOUND);
    assert_eq!(err.to_string(), "transaction not found");
    assert_eq!(err.exit_code(), 1);
}
