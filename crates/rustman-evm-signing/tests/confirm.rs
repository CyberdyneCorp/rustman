//! The confirmation (task 5.4): its fields, how each chain is confirmed
//! (yes, typed chain name, `--yes-local`), the refusals without a
//! terminal, and the send step refusing a confirmation of another
//! transaction.

use std::path::Path;
use std::sync::Mutex;

use alloy_primitives::{Address, U256};
use rustman_core::Secret;
use rustman_evm::rpc::RpcFuture;
use rustman_evm::{ChainRegistry, EvmSettings};
use rustman_evm_signing::confirm::{DECLINED, Mode, mode};
use rustman_evm_signing::fees::{Fee, Fees};
use rustman_evm_signing::send::{Broadcast, sign_and_send};
use rustman_evm_signing::transaction::UnsignedTx;
use rustman_evm_signing::{
    Confirmation, Signers, Summary, UnlockContext, UnlockedSigner, Zeroizing, unlock,
};
use serde_json::json;

const BOB: &str = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";

fn settings(text: &str) -> EvmSettings {
    EvmSettings::from_table(&toml::from_str(text).unwrap()).unwrap()
}

fn fee(gwei_tenths: u64, suggested: bool) -> Fee {
    Fee {
        wei: U256::from(gwei_tenths * 100_000_000),
        suggested,
    }
}

fn summary(chain_id: u64) -> Summary {
    let chain = ChainRegistry::built_in().get(chain_id);
    let to: Address = "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238"
        .parse()
        .unwrap();
    Summary {
        chain,
        signer_id: "deployer".to_owned(),
        signer: "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
            .parse()
            .unwrap(),
        to_name: Some("usdc.eth".to_owned()),
        call: format!("transfer(to={BOB}, amount=10000000)"),
        tx: UnsignedTx {
            chain_id,
            nonce: 4,
            to,
            value: U256::ZERO,
            input: vec![0xa9, 0x05, 0x9c, 0xbb],
            gas_limit: U256::from(55_317),
            fees: Fees::Eip1559 {
                max_fee: fee(165, true),
                max_priority_fee: fee(15, true),
            },
        },
        reverted: None,
    }
}

#[test]
fn the_confirmation_lists_every_required_field() {
    // Scenario "Confirmation content".
    let shown = summary(11_155_111).render();
    assert_eq!(
        shown,
        format!(
            "  chain        Sepolia (11155111)\n\
             \x20 signer       0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 (deployer)\n\
             \x20 to           0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238 (usdc.eth)\n\
             \x20 call         transfer(to={BOB}, amount=10000000)\n\
             \x20 value        0 ETH\n\
             \x20 gas          55317\n\
             \x20 max fee      16.5 gwei (suggested)\n\
             \x20 priority fee 1.5 gwei (suggested)\n\
             \x20 nonce        4\n\
             \x20 max cost     0.0009127305 ETH\n"
        )
    );
    let mut forced = summary(11_155_111);
    forced.reverted = Some("reverted: InsufficientBalance(5, 10)".to_owned());
    assert!(
        forced
            .render()
            .starts_with("WARNING: SIMULATION REVERTED: reverted: InsufficientBalance(5, 10)\n")
    );
}

#[test]
fn legacy_transactions_show_the_gas_price() {
    let mut legacy = summary(31_337);
    legacy.tx.fees = Fees::Legacy {
        gas_price: fee(200, false),
    };
    let shown = legacy.render();
    assert!(shown.contains("  gas price    20 gwei\n"), "{shown}");
    assert!(!shown.contains("max fee"), "{shown}");
}

#[test]
fn each_chain_has_its_way_of_confirming() {
    let plain = settings("");
    let registry = ChainRegistry::built_in();
    let mainnet = registry.get(1);
    let sepolia = registry.get(11_155_111);
    let local = registry.get(31_337);
    assert_eq!(
        mode(&mainnet, &plain, true, false).unwrap(),
        Mode::Typed("Ethereum Mainnet".to_owned())
    );
    assert_eq!(mode(&sepolia, &plain, true, false).unwrap(), Mode::YesNo);
    assert_eq!(mode(&local, &plain, true, false).unwrap(), Mode::YesNo);
    assert_eq!(
        mode(&local, &plain, false, true).unwrap(),
        Mode::LocalHeadless
    );
    // --yes-local never skips the prompt of another chain.
    assert_eq!(mode(&sepolia, &plain, true, true).unwrap(), Mode::YesNo);
    let protected = settings("protected_chains = [11155111]\nheadless_signing_chains = [1337]");
    assert_eq!(
        mode(&sepolia, &protected, true, false).unwrap(),
        Mode::Typed("Sepolia".to_owned())
    );
    let dev = registry.get(1337);
    assert_eq!(
        mode(&dev, &protected, false, true).unwrap(),
        Mode::LocalHeadless
    );
}

#[test]
fn without_a_terminal_only_local_chains_sign_and_only_with_yes_local() {
    // Scenario "CI against Sepolia refused".
    let plain = settings("");
    let registry = ChainRegistry::built_in();
    let err = mode(&registry.get(11_155_111), &plain, false, true).unwrap_err();
    assert_eq!(err.exit_code(), 4);
    assert_eq!(
        err.to_string(),
        "signing on Sepolia (11155111) needs an interactive confirmation; without a terminal only chain 31337 and [evm] headless_signing_chains can be signed on, with --yes-local"
    );
    let err = mode(&registry.get(31_337), &plain, false, false).unwrap_err();
    assert_eq!(err.exit_code(), 4);
    assert_eq!(
        err.to_string(),
        "no terminal to confirm on; add --yes-local to sign on Anvil/Hardhat local (31337) without a prompt"
    );
    let err = Confirmation::local_headless(&summary(11_155_111), &plain).unwrap_err();
    assert_eq!(err.exit_code(), 4);
    assert!(Confirmation::local_headless(&summary(31_337), &plain).is_ok());
}

#[test]
fn answers_other_than_yes_and_wrong_chain_names_cancel() {
    // Scenarios "Declined" and "Typed chain name".
    let plain = settings("");
    let mainnet = summary(1);
    for answer in ["", "n", "no", "sure"] {
        let err = Confirmation::interactive(&mainnet, &plain, answer, None).unwrap_err();
        assert_eq!(err.to_string(), DECLINED);
        assert_eq!(err.exit_code(), 4);
    }
    let err = Confirmation::interactive(&mainnet, &plain, "y", Some("mainnet")).unwrap_err();
    assert_eq!(err.exit_code(), 4);
    assert_eq!(
        err.to_string(),
        "the chain name did not match 'Ethereum Mainnet'; cancelled; nothing was signed or sent"
    );
    let err = Confirmation::interactive(&mainnet, &plain, "yes", None).unwrap_err();
    assert_eq!(err.exit_code(), 4);
    assert!(Confirmation::interactive(&mainnet, &plain, "Y", Some("Ethereum Mainnet")).is_ok());
    assert!(Confirmation::interactive(&summary(5), &plain, "yes", None).is_ok());
}

#[test]
fn a_protected_chain_needs_its_name_whatever_the_caller_asked() {
    // Regression: the caller's mode decided whether the name was checked,
    // so a front end passing a yes/no mode for chain 1 skipped it.
    let protected = settings("protected_chains = [11155111]");
    for summary in [summary(1), summary(11_155_111)] {
        let err = Confirmation::interactive(&summary, &protected, "yes", None).unwrap_err();
        assert_eq!(err.exit_code(), 4, "{}", summary.chain.label());
    }
}

#[test]
fn a_protected_chain_listed_as_headless_still_needs_its_name() {
    // Regression: with chain 1 in headless_signing_chains, --yes-local
    // signed on mainnet without a prompt.
    let overlap = settings(
        "headless_signing_chains = [1, 11155111]
protected_chains = [11155111]",
    );
    let registry = ChainRegistry::built_in();
    for id in [1, 11_155_111] {
        let chain = registry.get(id);
        assert!(!rustman_evm_signing::confirm::is_local(id, &overlap));
        assert_eq!(
            mode(&chain, &overlap, true, true).unwrap(),
            Mode::Typed(chain.name.clone())
        );
        let err = mode(&chain, &overlap, false, true).unwrap_err();
        assert_eq!(err.exit_code(), 4);
        assert!(
            err.to_string()
                .contains("needs an interactive confirmation"),
            "{err}"
        );
        let err = Confirmation::local_headless(&summary(id), &overlap).unwrap_err();
        assert_eq!(err.exit_code(), 4);
    }
}

#[test]
fn a_newline_in_a_value_cannot_draw_a_fake_field() {
    // Regression: a chain name or decoded argument with a newline drew a
    // line that looked like another field.
    let mut forged = summary(31_337);
    forged.chain.name = "x\n  value        0 ETH".to_owned();
    forged.call = "transfer(to=\n  max cost     0 ETH)".to_owned();
    let shown = forged.render();
    assert_eq!(
        shown.lines().filter(|l| l.starts_with("  value")).count(),
        1,
        "{shown}"
    );
    assert_eq!(
        shown
            .lines()
            .filter(|l| l.starts_with("  max cost"))
            .count(),
        1,
        "{shown}"
    );
    assert!(shown.contains("x\\u{a}  value"), "{shown}");
}

/// Records what it was asked to send and answers with `hash`.
struct Node {
    sent: Mutex<Vec<String>>,
}

impl Broadcast for Node {
    fn send_raw<'a>(&'a self, raw: String) -> RpcFuture<'a> {
        self.sent.lock().unwrap().push(raw);
        Box::pin(async { Ok(json!("0x00")) })
    }
}

fn anvil0() -> UnlockedSigner {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let table: toml::Table =
        toml::from_str("[signers.dev]\nkeystore = \"keystores/anvil0.json\"\n").unwrap();
    let signers = Signers::from_settings(&EvmSettings::from_table(&table).unwrap()).unwrap();
    let no_prompt = |_: &str| Ok(None);
    let ctx = UnlockContext {
        root: &fixtures,
        password: Some(Secret::new(
            "pw",
            Zeroizing::new("rustman-test-password".to_owned()),
        )),
        prompt: &no_prompt,
        keychain: None,
    };
    unlock(signers.get("dev").unwrap(), &ctx).unwrap()
}

#[tokio::test]
async fn a_confirmation_signs_only_the_transaction_it_confirmed() {
    let confirmed = summary(31_337);
    let confirmation = Confirmation::local_headless(&confirmed, &settings("")).unwrap();
    let mut other = confirmed.tx.clone();
    other.value = U256::from(1);
    let node = Node {
        sent: Mutex::new(Vec::new()),
    };
    let err = sign_and_send(&other, &anvil0(), confirmation, &node)
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 4);
    assert!(node.sent.lock().unwrap().is_empty(), "nothing was sent");
    // The right one is signed and sent; a node answering another hash is
    // an RPC error.
    let confirmation = Confirmation::local_headless(&confirmed, &settings("")).unwrap();
    let err = sign_and_send(&confirmed.tx, &anvil0(), confirmation, &node)
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 3);
    assert_eq!(node.sent.lock().unwrap().len(), 1);
}

#[test]
fn a_protected_chain_without_a_name_is_never_confirmed() {
    // Regression: a protected chain configured with an empty name was
    // confirmed by typing nothing.
    let protected = settings("protected_chains = [4242]");
    let mut nameless = summary(4242);
    nameless.chain.name = "  ".to_owned();
    for typed in [None, Some(""), Some("  ")] {
        let err = Confirmation::interactive(&nameless, &protected, "yes", typed).unwrap_err();
        assert_eq!(err.exit_code(), 4);
        assert_eq!(
            err.to_string(),
            "protected chain 4242 has no name to type; set [evm.chains.4242] name; cancelled; nothing was signed or sent"
        );
    }
}
