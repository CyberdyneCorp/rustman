//! The examples in docs/evm.md parse and load (task 1.4): the `[evm]`
//! table, the pinned environment and every request file, and the messages
//! the page quotes are the ones the code produces.

use std::fs;
use std::path::{Path, PathBuf};

use rustman_core::environment;
use rustman_core::model::{DEPLOYMENT_OUT_OF_SCOPE, KindRegistry, Request, SIGNING_UNAVAILABLE};
use rustman_core::settings::Settings;
use rustman_evm::abi::AbiCache;
use rustman_evm::blocklist::SEND_REFUSED;
use rustman_evm::call::OVERRIDES_UNSUPPORTED;
use rustman_evm::ens::ENS_NEEDS_MAINNET;
use rustman_evm::explorer::FETCH_DISABLED;
use rustman_evm::fork::{ANVIL_NOT_FOUND, TEMP_MARKER};
use rustman_evm::tx::{NO_DECIMALS, TX_NOT_FOUND};
use rustman_evm::{ChainRegistry, EvmSettings};

fn page() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/evm.md");
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

/// Every fenced block of `lang`.
fn blocks(text: &str, lang: &str) -> Vec<String> {
    let fence = format!("```{lang}");
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim_end() == fence {
            let block: String = lines
                .by_ref()
                .take_while(|l| l.trim_end() != "```")
                .map(|l| format!("{l}\n"))
                .collect();
            out.push(block);
        }
    }
    out
}

#[test]
fn the_evm_table_example_parses_without_warnings() {
    let tables = blocks(&page(), "toml");
    assert_eq!(tables.len(), 1);
    let (settings, warnings) = Settings::parse(Path::new("rustman.toml"), &tables[0]).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let evm = EvmSettings::from_table(&settings.evm).unwrap();
    assert!(evm.allow_explorer_fetch);
    let registry = ChainRegistry::with_overrides(&evm).unwrap();
    assert_eq!(registry.get(999).label(), "My L2 (999)");
    assert!(evm.explorer(1).is_some());
}

#[test]
fn the_pinned_environment_example_parses() {
    let yaml = blocks(&page(), "yaml");
    let env = yaml
        .iter()
        .find(|b| b.starts_with("chain_id:"))
        .expect("the environment");
    let parsed =
        environment::parse(Path::new("environments/sepolia.yaml"), "sepolia", env).unwrap();
    assert_eq!(parsed.chain_id, Some(11_155_111));
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/abi")
        .join(name)
}

#[test]
fn every_request_example_loads_and_passes_the_abi_checks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let artifact = root.join("out/Vault.sol/Vault.json");
    fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    fs::copy(fixture("foundry-Vault.json"), artifact).unwrap();
    let requests: Vec<String> = blocks(&page(), "yaml")
        .into_iter()
        .filter(|b| b.contains("kind: evm-"))
        .collect();
    assert_eq!(requests.len(), 5);
    let cache = AbiCache::new();
    for text in requests {
        let file = Path::new("requests/example.rman.yaml");
        let request = KindRegistry::default()
            .load(file, &text)
            .unwrap_or_else(|e| panic!("{e}\n{text}"));
        let Request::Evm(evm) = request else {
            panic!("an evm request");
        };
        rustman_evm::load::check(root, file, &evm, &cache)
            .unwrap_or_else(|e| panic!("{e}\n{text}"));
    }
}

#[test]
fn quoted_messages_match_the_code() {
    let text = page();
    for message in [
        FETCH_DISABLED,
        DEPLOYMENT_OUT_OF_SCOPE,
        SIGNING_UNAVAILABLE,
        NO_DECIMALS,
        TX_NOT_FOUND,
        "chain mismatch: environment 'sepolia' expects 11155111 (Sepolia), endpoint reports 1 (Ethereum Mainnet)",
        "function 'transfer' is overloaded; use a full signature such as 'transfer(address,uint256)'",
        OVERRIDES_UNSUPPORTED,
        ENS_NEEDS_MAINNET,
        ANVIL_NOT_FOUND,
        SEND_REFUSED,
        TEMP_MARKER,
        "tracing not supported by geth",
        "polling every 2s (no WebSocket)",
        rustman_evm::query::RANGE_TOO_LARGE,
        "evm output 'balance': expected >= 1 ETH, got 0.5 ETH",
        "evm-tx: signing is not available yet; nothing was signed or sent",
    ] {
        assert!(
            text.contains(message),
            "docs/evm.md does not quote: {message}"
        );
    }
}

#[test]
fn the_assertion_and_capture_examples_load() {
    use rustman_core::model::{Assertion, CaptureSource};
    let examples: Vec<String> = blocks(&page(), "yaml")
        .into_iter()
        .filter(|b| b.contains("kind: evm-"))
        .filter(|b| b.contains("assertions:") || b.contains("captures:"))
        .collect();
    assert_eq!(examples.len(), 2);
    let file = Path::new("requests/example.rman.yaml");
    let Request::Evm(treasury) = KindRegistry::default().load(file, &examples[0]).unwrap() else {
        panic!("an evm request");
    };
    let common = treasury.common();
    let evm = common
        .assertions
        .iter()
        .filter(|a| matches!(a.get(), Assertion::Evm(_)))
        .count();
    assert_eq!(evm, 3);
    assert_eq!(common.captures[0].from, CaptureSource::EvmOutput);
    let Request::Evm(owner) = KindRegistry::default().load(file, &examples[1]).unwrap() else {
        panic!("an evm request");
    };
    assert_eq!(owner.common().captures[0].var, "owner");
}

/// The failure messages the page quotes are the ones the assertions give.
#[test]
fn the_quoted_failure_messages_are_produced() {
    use alloy_dyn_abi::DynSolValue;
    use alloy_primitives::U256;
    use rustman_core::model::{Checked, EvmAssertion};
    use rustman_evm::assert::{EvmFacts, evaluate};
    use rustman_evm::codec::{NamedValue, Revert};
    let check = |yaml: &str, facts: &EvmFacts<'_>| {
        let a: Checked<EvmAssertion> =
            rustman_core::format::from_yaml(Path::new("a.yaml"), yaml).unwrap();
        evaluate(&a, facts).message
    };
    let native = rustman_evm::Denomination::default();
    let outputs = [NamedValue {
        name: "balance".into(),
        ty: "uint256".into(),
        value: DynSolValue::Uint(U256::from(500_000_000_000_000_000u64), 256),
    }];
    let returned = EvmFacts {
        outputs: Some(&outputs),
        revert: None,
        events: &[],
        gas: Some(U256::from(21_000)),
        native: &native,
        token: None,
    };
    let panic = Revert::Panic(U256::from(0x12));
    let reverted = EvmFacts {
        outputs: None,
        revert: Some(&panic),
        ..returned
    };
    let text = page();
    for message in [
        check("{output: balance, gte: \"1 ether\"}", &returned),
        check("{reverts_with: InsufficientBalance}", &reverted),
        check("{gas_lt: 21000}", &returned),
        check("{output: balance, gte: 1}", &reverted),
    ] {
        assert!(
            text.contains(&message),
            "docs/evm.md does not quote: {message}"
        );
    }
}

/// The blockchain GraphQL examples (task 7.5): every graphql request file
/// loads and its document validates against the geth or subgraph fixture
/// schema of its preset, the query blocks validate against the subgraph
/// fixture, and the quoted messages are the code's.
#[test]
fn the_graphql_preset_examples_parse_against_the_fixture_schemas() {
    use rustman_core::model::GraphqlPreset;
    use rustman_graphql::GraphqlSchema;
    let schema = |name: &str| {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("tests/fixtures/graphql/{name}.introspection.json"));
        GraphqlSchema::from_file(&path, name).unwrap()
    };
    let (geth, subgraph) = (schema("geth"), schema("subgraph"));
    let valid = |document: &str, schema: &GraphqlSchema| {
        let validation =
            rustman_graphql::validate(document, Some(schema), None, &serde_json::Map::new());
        let errors: Vec<String> = validation.errors().map(ToString::to_string).collect();
        assert!(errors.is_empty(), "{errors:?}\n{document}");
    };
    let text = page();
    let requests: Vec<String> = blocks(&text, "yaml")
        .into_iter()
        .filter(|b| b.contains("kind: graphql"))
        .collect();
    assert_eq!(requests.len(), 2);
    for yaml in &requests {
        let file = Path::new("requests/example.rman.yaml");
        let Request::Graphql(graphql) = KindRegistry::default().load(file, yaml).unwrap() else {
            panic!("a graphql request");
        };
        let document = graphql.document.as_deref().unwrap();
        match graphql.preset {
            Some(GraphqlPreset::Geth) => valid(document, &geth),
            _ => valid(document, &subgraph),
        }
    }
    let queries = blocks(&text, "graphql");
    assert_eq!(queries.len(), 1);
    valid(&queries[0], &subgraph);
    for message in [
        rustman_graphql::preset::NOT_GETH,
        rustman_graphql::preset::NOT_SUBGRAPH,
        rustman_graphql::META_QUERY,
        rustman_core::assertions::SUBGRAPH_NO_RPC,
        rustman_core::run::LAG_NO_RPC,
        "subgraph lag 120 blocks exceeds 50",
        "stopped at --max-pages 20",
        "1000 (18 decimals)",
    ] {
        assert!(
            text.contains(message),
            "docs/evm.md does not quote: {message}"
        );
    }
}
