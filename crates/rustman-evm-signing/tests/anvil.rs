//! Signing and sending against a local Anvil (tasks 5.3 and 5.5): two
//! sequential transactions of one signer, EIP-1559 with suggested fees
//! then legacy with the gas price, nonces from the pending count then the
//! in-run counter, receipts decoded with the request's ABI, `evm`
//! assertions on them, and audit records without key material.
//!
//! Anvil's well-known first dev account, from an encrypted test keystore.
//! The fixture contract's runtime code is installed with `anvil_setCode`,
//! so neither forge nor solc is needed. These tests only run with
//! `RUSTMAN_ANVIL=1` (or `RUSTMAN_REQUIRE_ANVIL=1`, the CI Anvil job,
//! where a missing `anvil` fails them).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alloy_json_abi::JsonAbi;
use alloy_primitives::{Address, U256};
use rustman_core::Secret;
use rustman_core::model::{Assertion, TxType};
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedRequest, TlsFiles};
use rustman_evm::call::{BlockSel, CallRequest};
use rustman_evm::codec::encode_call;
use rustman_evm::fork::{Anvil, AnvilOptions};
use rustman_evm::rpc::call;
use rustman_evm::{ChainRegistry, Denomination, EvmSettings, JsonrpcTransport};
use rustman_evm_signing::fees::{FeeInputs, fees, gas_limit};
use rustman_evm_signing::gate::simulate;
use rustman_evm_signing::nonce::Nonces;
use rustman_evm_signing::receipt::{self, Mined};
use rustman_evm_signing::send::{Broadcasted, Wait, Waited, sign_and_send, wait_for_receipt};
use rustman_evm_signing::transaction::UnsignedTx;
use rustman_evm_signing::{
    Confirmation, Signers, Summary, SummaryParts, UnlockContext, UnlockedSigner, Zeroizing, unlock,
};
use rustman_http::{HttpEngine, SendOptions};
use serde_json::{Value, json};

const PASSWORD: &str = "rustman-test-password";
const ANVIL0_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const COUNTER: &str = "0x00000000000000000000000000000000C0C0C0C0";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// `anvil` on `PATH` when the Anvil tests are enabled.
fn anvil_program() -> Option<PathBuf> {
    let require = std::env::var("RUSTMAN_REQUIRE_ANVIL").ok();
    let opt_in = std::env::var("RUSTMAN_ANVIL").ok();
    if require.as_deref() != Some("1") && opt_in.as_deref() != Some("1") {
        eprintln!("set RUSTMAN_ANVIL=1 to run the Anvil tests: skipping");
        return None;
    }
    let file = if cfg!(windows) { "anvil.exe" } else { "anvil" };
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(file))
            .find(|p| p.is_file())
    });
    assert!(
        found.is_some() || require.as_deref() != Some("1"),
        "anvil is required (RUSTMAN_REQUIRE_ANVIL=1) but not on PATH"
    );
    found
}

fn transport(url: &str) -> JsonrpcTransport {
    let endpoint = ResolvedRequest {
        method: "POST".to_owned(),
        url: Resolved::literal(url),
        query: Vec::new(),
        headers: Vec::new(),
        body: ResolvedBody::None,
        auth: None,
        settings: Default::default(),
        tls: TlsFiles::default(),
        secrets: Vec::new(),
    };
    JsonrpcTransport::new(
        endpoint,
        Arc::new(HttpEngine::new()),
        SendOptions::default(),
    )
}

fn signer() -> UnlockedSigner {
    let table: toml::Table =
        toml::from_str("[signers.dev]\nkeystore = \"keystores/anvil0.json\"\n").unwrap();
    let signers = Signers::from_settings(&EvmSettings::from_table(&table).unwrap()).unwrap();
    let no_prompt = |_: &str| Ok(None);
    let root = fixtures();
    let ctx = UnlockContext {
        root: &root,
        password: Some(Secret::new("pw", Zeroizing::new(PASSWORD.to_owned()))),
        prompt: &no_prompt,
        keychain: None,
    };
    unlock(signers.get("dev").unwrap(), &ctx).unwrap()
}

fn counter_abi() -> JsonAbi {
    let text = std::fs::read_to_string(fixtures().join("counter/Counter.abi.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// Anvil with the counter installed at [`COUNTER`].
async fn node(program: &Path) -> (Anvil, JsonrpcTransport) {
    let mut options = AnvilOptions::new(program.to_owned());
    options.port = 0;
    let anvil = Anvil::start(&options).await.unwrap();
    let rpc = transport(&anvil.url());
    let code = std::fs::read_to_string(fixtures().join("counter/Counter.runtime.hex")).unwrap();
    call(&rpc, "anvil_setCode", json!([COUNTER, code.trim()]))
        .await
        .unwrap();
    (anvil, rpc)
}

/// Prepare, confirm (`--yes-local`), sign, send and wait for one `add`.
async fn add(
    rpc: &JsonrpcTransport,
    signer: &UnlockedSigner,
    nonces: &Nonces,
    amount: u64,
    tx_type: TxType,
) -> (Summary, Mined) {
    let abi = counter_abi();
    let function = abi.function("add").unwrap()[0].clone();
    let to: Address = COUNTER.parse().unwrap();
    let data = encode_call(&function, &[json!(amount.to_string())]).unwrap();
    let request = CallRequest {
        to,
        from: Some(signer.address()),
        value: None,
        data: data.clone(),
        block: BlockSel::Tag("pending".to_owned()),
        overrides: None,
    };
    let simulation = simulate(rpc, &request, &function, Some(&abi))
        .await
        .unwrap();
    assert_eq!(simulation.revert(), None);
    let native = Denomination::default();
    let gas = gas_limit(None, simulation.gas_estimate(), &native).unwrap();
    let inputs = FeeInputs {
        tx_type,
        ..FeeInputs::default()
    };
    let fees = fees(rpc, &inputs, &native).await.unwrap();
    let nonce = nonces
        .next(rpc, 31_337, signer.address(), None)
        .await
        .unwrap();
    let summary = Summary::new(SummaryParts {
        chain: ChainRegistry::built_in().get(31_337),
        signer_id: "dev".to_owned(),
        signer: signer.address(),
        to_name: None,
        call: format!("add(amount={amount})"),
        tx: UnsignedTx {
            chain_id: 31_337,
            nonce,
            to,
            value: U256::ZERO,
            input: data,
            gas_limit: gas,
            fees,
        },
        reverted: None,
    })
    .unwrap();
    let settings = EvmSettings::default();
    let confirmation = Confirmation::local_headless(&summary, &settings).unwrap();
    let hash = sign_and_send(&summary.tx, signer, confirmation, rpc)
        .await
        .unwrap();
    let Broadcasted::Accepted(hash) = hash else {
        panic!("{hash:?}");
    };
    nonces.sent(31_337, signer.address(), nonce);
    let wait = Wait {
        poll: Duration::from_millis(50),
        ..Wait::default()
    };
    let Waited::Mined(receipt) = wait_for_receipt(rpc, hash, &wait).await else {
        panic!("Anvil mines at once");
    };
    let mined = receipt::decode(rpc, &summary, hash, &abi, &receipt)
        .await
        .unwrap();
    (summary, mined)
}

#[tokio::test(flavor = "multi_thread")]
async fn two_sequential_transactions_are_signed_sent_and_audited() {
    let Some(program) = anvil_program() else {
        return;
    };
    let (anvil, rpc) = node(&program).await;
    let signer = signer();
    let nonces = Nonces::new();

    let (first, mined) = add(&rpc, &signer, &nonces, 2, TxType::Eip1559).await;
    assert!(mined.succeeded(), "{:?}", mined.report);
    assert_eq!(first.tx.nonce, 0);
    assert!(matches!(
        first.tx.fees,
        rustman_evm_signing::fees::Fees::Eip1559 { max_fee, .. } if max_fee.suggested
    ));
    assert_eq!(mined.report.from, signer.address());
    assert_eq!(mined.events.len(), 1);
    assert_eq!(mined.events[0].to_string().split('(').next(), Some("Added"));

    // The second transaction uses the first's nonce + 1 without asking the
    // node (which would also answer 1 here, as Anvil mined the first).
    let (second, mined) = add(&rpc, &signer, &nonces, 3, TxType::Legacy).await;
    assert!(mined.succeeded());
    assert_eq!(second.tx.nonce, 1);
    assert_eq!(mined.report.nonce, 1);

    // Receipt document and assertions.
    let document = receipt::document(&mined);
    assert_eq!(document["receipt"]["status"], "success");
    assert_eq!(document["events"][0]["fields"]["number"], "5", "{document}");
    let assertions: Vec<Assertion> = serde_json::from_value(json!([
        {"evm": {"emits": "Added(address,uint256,uint256)", "where": {"amount": "3"}}},
        {"evm": {"gas_lt": 100_000}},
    ]))
    .unwrap();
    let results = receipt::evm_assertions(&assertions, &mined, &Denomination::default());
    assert!(results.iter().all(|r| r.passed()), "{results:?}");

    // The audit record: what was sent, never the signed bytes or the key.
    let audit = receipt::audit(&second, mined.report.hash.parse().unwrap(), Some(&mined));
    assert_eq!(audit["chain_id"], 31_337);
    assert_eq!(
        audit["signer"]["address"],
        signer.address().to_checksum(None)
    );
    assert_eq!(audit["hash"], mined.report.hash);
    assert_eq!(audit["call"], "add(amount=3)");
    assert_eq!(audit["status"], "success");
    assert_eq!(audit["fees"]["type"], "legacy");
    assert!(audit["gas_used"].as_str().is_some());
    assert_no_key_material(&audit);

    let number = call(
        &rpc,
        "eth_call",
        json!([{"to": COUNTER, "data": "0x8381f58a"}, "latest"]),
    )
    .await
    .unwrap();
    assert_eq!(
        number
            .as_str()
            .map(|n| U256::from_str_radix(&n[2..], 16).unwrap()),
        Some(U256::from(5))
    );
    anvil.stop().await;
}

fn assert_no_key_material(audit: &Value) {
    let text = audit.to_string().to_lowercase();
    assert!(!text.contains(ANVIL0_KEY), "{text}");
    assert!(!text.contains(PASSWORD), "{text}");
    for field in ["raw", "signature", "r", "s", "v"] {
        assert!(audit.get(field).is_none(), "{field} in {text}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_transaction_that_is_never_mined_times_out() {
    let Some(program) = anvil_program() else {
        return;
    };
    let (anvil, rpc) = node(&program).await;
    call(&rpc, "evm_setAutomine", json!([false])).await.unwrap();
    let signer = signer();
    let abi = counter_abi();
    let function = abi.function("add").unwrap()[0].clone();
    let tx = UnsignedTx {
        chain_id: 31_337,
        nonce: 0,
        to: COUNTER.parse().unwrap(),
        value: U256::ZERO,
        input: encode_call(&function, &[json!("1")]).unwrap(),
        gas_limit: U256::from(100_000),
        fees: fees(&rpc, &FeeInputs::default(), &Denomination::default())
            .await
            .unwrap(),
    };
    let summary = Summary::new(SummaryParts {
        chain: ChainRegistry::built_in().get(31_337),
        signer_id: "dev".to_owned(),
        signer: signer.address(),
        to_name: None,
        call: "add(amount=1)".to_owned(),
        tx,
        reverted: None,
    })
    .unwrap();
    let confirmation = Confirmation::local_headless(&summary, &EvmSettings::default()).unwrap();
    let hash = sign_and_send(&summary.tx, &signer, confirmation, &rpc)
        .await
        .unwrap();
    let Broadcasted::Accepted(hash) = hash else {
        panic!("{hash:?}");
    };
    let wait = Wait {
        confirmations: 1,
        timeout: Duration::from_millis(600),
        poll: Duration::from_millis(100),
    };
    assert_eq!(wait_for_receipt(&rpc, hash, &wait).await, Waited::TimedOut);
    let audit = receipt::audit(&summary, hash, None);
    assert_eq!(audit["status"], "pending");
    assert_no_key_material(&audit);
    anvil.stop().await;
}
