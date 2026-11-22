//! The gate preconditions (task 5.2): one test per refusal path, each a
//! refusal with exit code 4 and its exact message, against recorded node
//! answers.

use alloy_json_abi::JsonAbi;
use alloy_primitives::{Address, U256, keccak256};
use rustman_evm::call::{BlockSel, CallRequest};
use rustman_evm::recorded::RecordedCall;
use rustman_evm::{ChainRegistry, EvmError, EvmSettings, Recorded};
use rustman_evm_signing::RAW_KEY_REFUSED;
use rustman_evm_signing::gate::{
    NO_SIGNER, SIGN_FLAG_MISSING, SIGNING_DISABLED, check_chain, check_enabled, check_sign_flag,
    check_simulation, require_pin, select_signer, simulate,
};
use serde_json::{Value, json};

const KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const SIGNER: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
const TOKEN: &str = "0x5FbDB2315678afecb367f032d93F642f64180aa3";

fn assert_refused(result: Result<impl std::fmt::Debug, EvmError>, message: &str) {
    let err = result.unwrap_err();
    assert_eq!(err.to_string(), message);
    assert_eq!(err.exit_code(), 4, "{message}");
}

fn settings(text: &str) -> EvmSettings {
    EvmSettings::from_table(&toml::from_str(text).unwrap()).unwrap()
}

#[test]
fn a_workspace_without_allow_signing_is_refused() {
    assert_refused(check_enabled(&settings("")), SIGNING_DISABLED);
    assert_refused(
        check_enabled(&settings("allow_signing = false")),
        SIGNING_DISABLED,
    );
    assert_eq!(
        SIGNING_DISABLED,
        "signing is disabled for this workspace; set [evm] allow_signing = true"
    );
    assert!(check_enabled(&settings("allow_signing = true")).is_ok());
}

#[test]
fn a_protected_chain_in_headless_signing_chains_is_a_config_error() {
    // Regression: chain 1 in headless_signing_chains let --yes-local sign
    // on mainnet without a prompt.
    for text in [
        "allow_signing = true\nheadless_signing_chains = [1]",
        "allow_signing = true\nheadless_signing_chains = [1337]\nprotected_chains = [1337]",
    ] {
        let err = check_enabled(&settings(text)).unwrap_err();
        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(err.to_string().contains("is protected"), "{err}");
    }
    let fine = "allow_signing = true\nheadless_signing_chains = [1337]";
    assert!(check_enabled(&settings(fine)).is_ok());
}

#[test]
fn a_public_mainnet_in_headless_signing_chains_is_a_config_error() {
    // Regression: Base, Arbitrum, Polygon... were accepted, so
    // `--sign --yes-local` in CI signed on a real-money chain without a
    // prompt.
    for (id, label) in [
        (8453, "Base (8453)"),
        (10, "OP Mainnet (10)"),
        (42_161, "Arbitrum One (42161)"),
        (137, "Polygon PoS (137)"),
    ] {
        let text = format!("allow_signing = true\nheadless_signing_chains = [1337, {id}]");
        let err = check_enabled(&settings(&text)).unwrap_err();
        assert_eq!(err.exit_code(), 2, "{err}");
        assert_eq!(
            err.to_string(),
            format!(
                "[evm] headless_signing_chains: {label} is a mainnet; only test and private chains can be signed on without a prompt; remove it"
            )
        );
    }
    // Testnets and unknown (private) ids are fine.
    let fine = "allow_signing = true\nheadless_signing_chains = [11155111, 17000, 31337, 424242]";
    assert!(check_enabled(&settings(fine)).is_ok());
}

#[test]
fn a_built_in_chain_keeps_its_name_and_currency_when_signing() {
    // Regression: [evm.chains.1] renamed mainnet and changed its decimals,
    // so the confirmation showed a wrong value and the name to type.
    use rustman_evm_signing::gate::signing_chain;
    let overrides = settings(
        "[chains.1]\nname = \"Local devnet\"\ndecimals = 24\nsymbol = \"X\"\nexplorer = \"https://eth.example\"\n[chains.999]\nname = \"My L2\"\ndecimals = 6\n",
    );
    let registry = ChainRegistry::with_overrides(&overrides).unwrap();
    let mainnet = signing_chain(&registry, 1);
    assert_eq!(mainnet.label(), "Ethereum Mainnet (1)");
    assert_eq!((mainnet.symbol.as_str(), mainnet.decimals), ("ETH", 18));
    assert_eq!(mainnet.explorer.as_deref(), Some("https://eth.example"));
    // A chain only the workspace defines is as configured.
    let custom = signing_chain(&registry, 999);
    assert_eq!((custom.name.as_str(), custom.decimals), ("My L2", 6));
}

#[test]
fn a_missing_sign_flag_is_refused() {
    assert_refused(check_sign_flag(false), SIGN_FLAG_MISSING);
    assert_eq!(SIGN_FLAG_MISSING, "add --sign to sign and send");
    assert!(check_sign_flag(true).is_ok());
}

#[test]
fn a_raw_key_as_signer_is_refused_from_the_flag_or_the_request() {
    assert_refused(select_signer(Some(KEY), None), RAW_KEY_REFUSED);
    assert_refused(select_signer(None, Some(&KEY[2..])), RAW_KEY_REFUSED);
    // A raw key in the request is refused even when --signer overrides it.
    assert_refused(select_signer(Some("ci"), Some(KEY)), RAW_KEY_REFUSED);
    assert_eq!(select_signer(Some("ci"), Some("deployer")).unwrap(), "ci");
    assert_eq!(select_signer(None, Some("deployer")).unwrap(), "deployer");
    let none = select_signer(None, None).unwrap_err();
    assert_eq!(none.to_string(), NO_SIGNER);
    assert_eq!(none.exit_code(), 2);
}

#[test]
fn the_chosen_signer_id_is_checked_and_never_echoed() {
    // Regression: a near-key (63 or 66 hex digits) or a seed phrase
    // passed as the signer was used as the id and echoed in errors.
    let near = [&KEY[..65], &KEY[2..], "0x00"];
    for value in near {
        let err = select_signer(Some(value), None).unwrap_err();
        assert!(!err.to_string().contains(value), "{err}");
    }
    let phrase = "test test test test test test test test test test test junk";
    assert_refused(select_signer(None, Some(phrase)), RAW_KEY_REFUSED);
    let padded = format!("0x00{}", &KEY[2..]);
    assert_refused(select_signer(Some(&padded), None), RAW_KEY_REFUSED);
    let err = select_signer(Some("deploy er"), None).unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert!(!err.to_string().contains("deploy er"), "{err}");
}

#[test]
fn an_environment_without_chain_id_is_refused() {
    assert_refused(
        require_pin(Some("sepolia"), None),
        "signing requires 'chain_id' in environment 'sepolia'",
    );
    assert_refused(
        require_pin(None, None),
        "signing requires an environment with 'chain_id'; select one with --env",
    );
    let pin = require_pin(Some("local"), Some(31_337)).unwrap();
    assert_eq!((pin.environment, pin.chain_id), ("local", 31_337));
}

fn node(calls: Value) -> Recorded {
    let calls: Vec<RecordedCall> = serde_json::from_value(calls).unwrap();
    Recorded::new("http://node.test", calls)
}

#[tokio::test]
async fn a_chain_mismatch_is_refused_from_a_fresh_chain_id() {
    let registry = ChainRegistry::built_in();
    let sepolia = node(json!([{"method": "eth_chainId", "result": "0xaa36a7"}]));
    let pin = require_pin(Some("local"), Some(31_337)).unwrap();
    assert_refused(
        check_chain(&sepolia, &registry, pin).await,
        "chain mismatch: environment 'local' expects 31337 (Anvil/Hardhat local), endpoint reports 11155111 (Sepolia)",
    );
    let anvil = node(json!([{"method": "eth_chainId", "result": "0x7a69"}]));
    let chain = check_chain(&anvil, &registry, pin).await.unwrap();
    assert_eq!(chain.id, 31_337);
    check_chain(&anvil, &registry, pin).await.unwrap();
    assert_eq!(
        anvil.methods(),
        ["eth_chainId", "eth_chainId"],
        "never cached"
    );
}

fn token_abi() -> JsonAbi {
    serde_json::from_value(json!([
        {"type": "function", "name": "transfer", "stateMutability": "nonpayable",
         "inputs": [{"name": "to", "type": "address"}, {"name": "amount", "type": "uint256"}],
         "outputs": [{"name": "", "type": "bool"}]},
        {"type": "error", "name": "InsufficientBalance",
         "inputs": [{"name": "available", "type": "uint256"}, {"name": "required", "type": "uint256"}]}
    ]))
    .unwrap()
}

fn transfer() -> CallRequest {
    CallRequest {
        to: TOKEN.parse().unwrap(),
        from: Some(SIGNER.parse::<Address>().unwrap()),
        value: None,
        data: vec![0xa9, 0x05, 0x9c, 0xbb],
        block: BlockSel::default(),
        overrides: None,
    }
}

fn insufficient_balance() -> String {
    let mut data = keccak256("InsufficientBalance(uint256,uint256)")[..4].to_vec();
    data.extend(U256::from(5).to_be_bytes::<32>());
    data.extend(U256::from(10).to_be_bytes::<32>());
    alloy_primitives::hex::encode_prefixed(data)
}

#[tokio::test]
async fn a_reverting_simulation_is_refused_unless_forced() {
    let revert =
        json!({"code": 3, "message": "execution reverted", "data": insufficient_balance()});
    let rpc = node(json!([
        {"method": "eth_call", "error": revert},
        {"method": "eth_estimateGas", "error": revert},
    ]));
    let abi = token_abi();
    let function = abi.function("transfer").unwrap()[0].clone();
    let simulation = simulate(&rpc, &transfer(), &function, Some(&abi))
        .await
        .unwrap();
    assert_refused(
        check_simulation(&simulation, false),
        "simulation reverted: InsufficientBalance(available=5, required=10); nothing was signed or sent (add --force-revert to sign anyway)",
    );
    assert!(
        check_simulation(&simulation, true).is_ok(),
        "--force-revert"
    );
    let seen = rpc.seen();
    assert_eq!(seen[0].1[0]["from"], SIGNER, "simulated from the signer");
    assert_eq!(seen[0].1[1], "pending", "at the pending block");
}

#[tokio::test]
async fn a_failing_estimate_is_refused_and_a_clean_simulation_passes() {
    let ok = "0x0000000000000000000000000000000000000000000000000000000000000001";
    let abi = token_abi();
    let function = abi.function("transfer").unwrap()[0].clone();
    let broke = node(json!([
        {"method": "eth_call", "result": ok},
        {"method": "eth_estimateGas", "error": {"code": -32000, "message": "insufficient funds for gas * price + value"}},
    ]));
    let simulation = simulate(&broke, &transfer(), &function, Some(&abi))
        .await
        .unwrap();
    assert_refused(
        check_simulation(&simulation, false),
        "simulation failed: eth_estimateGas: rpc error -32000: insufficient funds for gas * price + value; nothing was signed or sent (add --force-revert to sign anyway)",
    );
    let fine = node(json!([
        {"method": "eth_call", "result": ok},
        {"method": "eth_estimateGas", "result": "0xb411"},
    ]));
    let simulation = simulate(&fine, &transfer(), &function, Some(&abi))
        .await
        .unwrap();
    assert!(check_simulation(&simulation, false).is_ok());
    assert_eq!(simulation.revert(), None);
    assert_eq!(simulation.gas_estimate(), Some(U256::from(0xb411)));
}

#[tokio::test]
async fn a_transport_failure_is_not_a_simulation_result() {
    let abi = token_abi();
    let function = abi.function("transfer").unwrap()[0].clone();
    let down = node(json!([
        {"method": "eth_call", "error": {"code": -32603, "message": "internal error"}},
    ]));
    let err = simulate(&down, &transfer(), &function, Some(&abi))
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 3, "{err}");
}
