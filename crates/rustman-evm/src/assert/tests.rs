use alloy_dyn_abi::DynSolValue;
use alloy_json_abi::{Event, JsonAbi};
use alloy_primitives::{Address, I256, U256, address};
use rustman_core::model::{Checked, EvmAssertion};
use serde_json::json;

use super::*;
use crate::codec::{DecodedLog, NamedValue, Revert, decode_revert};

const ALICE: Address = address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045");

fn assertion(yaml: &str) -> EvmAssertion {
    let checked: Checked<EvmAssertion> =
        rustman_core::format::from_yaml(std::path::Path::new("a.yaml"), yaml).unwrap();
    checked.into_inner()
}

fn named(name: &str, value: DynSolValue) -> NamedValue {
    NamedValue {
        name: name.to_owned(),
        ty: String::new(),
        value,
    }
}

fn outputs() -> Vec<NamedValue> {
    let half_ether = U256::from(500_000_000_000_000_000u64);
    vec![
        named("balance", DynSolValue::Uint(half_ether, 256)),
        named("owner", DynSolValue::Address(ALICE)),
        named("name", DynSolValue::String("Test Token".into())),
        named("paused", DynSolValue::Bool(false)),
        named("delta", DynSolValue::Int(I256::try_from(-5).unwrap(), 256)),
        named("", DynSolValue::Bytes(vec![0xde, 0xad, 0xbe, 0xef])),
        named(
            "ids",
            DynSolValue::Array(vec![
                DynSolValue::Uint(U256::from(1), 256),
                DynSolValue::Uint(U256::from(7), 256),
            ]),
        ),
    ]
}

fn transfer() -> DecodedLog {
    let abi: JsonAbi = serde_json::from_value(json!([{
        "type": "event", "name": "Transfer", "anonymous": false, "inputs": [
            {"name": "from", "type": "address", "indexed": true},
            {"name": "to", "type": "address", "indexed": true},
            {"name": "value", "type": "uint256", "indexed": false}]
    }]))
    .unwrap();
    let event: Event = abi.events().next().unwrap().clone();
    DecodedLog {
        event,
        fields: vec![
            named("from", DynSolValue::Address(Address::ZERO)),
            named("to", DynSolValue::Address(ALICE)),
            named("value", DynSolValue::Uint(U256::from(10), 256)),
        ],
    }
}

struct Case {
    outputs: Option<Vec<NamedValue>>,
    revert: Option<Revert>,
    events: Vec<DecodedLog>,
    gas: Option<U256>,
    token: Option<crate::tx::TokenMeta>,
}

impl Case {
    fn returned() -> Self {
        Self {
            outputs: Some(outputs()),
            revert: None,
            events: vec![transfer()],
            gas: Some(U256::from(21_000)),
            token: None,
        }
    }

    fn reverted(revert: Revert) -> Self {
        Self {
            outputs: None,
            revert: Some(revert),
            events: Vec::new(),
            gas: None,
            token: None,
        }
    }

    fn check(&self, yaml: &str) -> AssertionResult {
        let native = Denomination::default();
        let facts = EvmFacts {
            outputs: self.outputs.as_deref(),
            revert: self.revert.as_ref(),
            events: &self.events,
            gas: self.gas,
            native: &native,
            token: self.token.as_ref(),
        };
        evaluate(&assertion(yaml), &facts)
    }
}

fn outcome(case: &Case, yaml: &str) -> Outcome {
    case.check(yaml).outcome
}

#[test]
fn balance_threshold_scenario() {
    let failed = Case::returned().check("{output: balance, gte: \"1 ether\"}");
    assert_eq!(failed.outcome, Outcome::Fail);
    assert_eq!(
        failed.message,
        "evm output 'balance': expected >= 1 ETH, got 0.5 ETH"
    );
}

#[test]
fn ordering_ops_on_quantities() {
    let case = Case::returned();
    for (yaml, expected) in [
        ("{output: balance, gt: \"0.4 ether\"}", Outcome::Pass),
        ("{output: balance, gte: \"500000000 gwei\"}", Outcome::Pass),
        ("{output: balance, lt: \"1 ether\"}", Outcome::Pass),
        ("{output: balance, lte: \"0.5 ether\"}", Outcome::Pass),
        ("{output: balance, lt: \"0x1\"}", Outcome::Fail),
        ("{output: 0, equals: \"500000000000000000\"}", Outcome::Pass),
        ("{output: balance, equals: 1}", Outcome::Fail),
        ("{output: delta, lt: 0}", Outcome::Pass),
        ("{output: delta, equals: -5}", Outcome::Pass),
        ("{output: name, gt: 1}", Outcome::Error),
    ] {
        assert_eq!(outcome(&case, yaml), expected, "{yaml}");
    }
}

#[test]
fn equals_and_contains_by_type() {
    let case = Case::returned();
    for (yaml, expected) in [
        (
            "{output: owner, equals: \"0xd8da6bf26964af9d7eed9e03e53415d37aa96045\"}",
            Outcome::Pass,
        ),
        (
            "{output: owner, equals: \"0x0000000000000000000000000000000000000001\"}",
            Outcome::Fail,
        ),
        ("{output: name, equals: \"Test Token\"}", Outcome::Pass),
        ("{output: name, contains: Token}", Outcome::Pass),
        ("{output: paused, equals: false}", Outcome::Pass),
        ("{output: 5, equals: \"0xDEADBEEF\"}", Outcome::Pass),
        ("{output: 5, contains: \"beef\"}", Outcome::Pass),
        ("{output: ids, contains: 7}", Outcome::Pass),
        ("{output: ids, contains: 8}", Outcome::Fail),
        ("{output: paused, contains: x}", Outcome::Error),
        ("{output: missing, equals: 1}", Outcome::Error),
    ] {
        assert_eq!(outcome(&case, yaml), expected, "{yaml}");
    }
}

#[test]
fn emits_with_and_without_where() {
    let case = Case::returned();
    let sig = "Transfer(address,address,uint256)";
    assert_eq!(
        outcome(&case, &format!("{{emits: \"{sig}\"}}")),
        Outcome::Pass
    );
    let spaced = "{emits: \"Transfer(address, address, uint256)\", where: {value: 10}}";
    assert_eq!(outcome(&case, spaced), Outcome::Pass);
    let to = format!("{{emits: \"{sig}\", where: {{to: \"{ALICE}\"}}}}");
    assert_eq!(outcome(&case, &to), Outcome::Pass);
    let wrong = format!("{{emits: \"{sig}\", where: {{value: 11}}}}");
    assert_eq!(outcome(&case, &wrong), Outcome::Fail);
    assert_eq!(
        outcome(&case, "{emits: \"Approval(address,address,uint256)\"}"),
        Outcome::Fail
    );
}

#[test]
fn reverts_and_reverts_with() {
    let custom: JsonAbi = serde_json::from_value(json!([{"type": "error", "name": "InsufficientBalance",
        "inputs": [{"name": "available", "type": "uint256"}, {"name": "required", "type": "uint256"}]}]))
    .unwrap();
    let mut data =
        alloy_primitives::keccak256("InsufficientBalance(uint256,uint256)")[..4].to_vec();
    data.extend(U256::from(5).to_be_bytes::<32>());
    data.extend(U256::from(10).to_be_bytes::<32>());
    let reverted = Case::reverted(decode_revert(Some(&custom), &data));
    // Scenario "Expected revert".
    assert_eq!(
        outcome(&reverted, "{reverts_with: InsufficientBalance}"),
        Outcome::Pass
    );
    assert_eq!(outcome(&reverted, "{reverts: true}"), Outcome::Pass);
    assert_eq!(outcome(&reverted, "{reverts: false}"), Outcome::Fail);
    assert_eq!(
        outcome(&reverted, "{reverts_with: Unauthorized}"),
        Outcome::Fail
    );
    let reason = Case::reverted(Revert::Reason("not owner".into()));
    assert_eq!(outcome(&reason, "{reverts_with: not owner}"), Outcome::Pass);
    let panic = Case::reverted(Revert::Panic(U256::from(0x12)));
    assert_eq!(outcome(&panic, "{reverts_with: Panic}"), Outcome::Pass);
    assert_eq!(
        outcome(&panic, "{reverts_with: \"Panic(0x12)\"}"),
        Outcome::Pass
    );
    let returned = Case::returned();
    assert_eq!(outcome(&returned, "{reverts: true}"), Outcome::Fail);
    assert_eq!(outcome(&returned, "{reverts: false}"), Outcome::Pass);
    assert_eq!(outcome(&returned, "{reverts_with: X}"), Outcome::Fail);
    assert_eq!(outcome(&reverted, "{output: 0, equals: 1}"), Outcome::Error);
}

#[test]
fn token_amounts_take_the_token_symbol_as_unit() {
    let usdc = Case {
        outputs: Some(vec![named(
            "",
            DynSolValue::Uint(U256::from(250_000_000u64), 256),
        )]),
        token: Some(crate::tx::TokenMeta {
            decimals: Some(6),
            symbol: Some("USDC".into()),
        }),
        ..Case::reverted(Revert::Empty)
    };
    let usdc = Case {
        revert: None,
        ..usdc
    };
    assert_eq!(
        outcome(&usdc, "{output: 0, gte: \"250 USDC\"}"),
        Outcome::Pass
    );
    let failed = usdc.check("{output: 0, gt: \"250.5 usdc\"}");
    assert_eq!(
        failed.message,
        "evm output '0': expected > 250.5 USDC, got 250 USDC"
    );
    assert_eq!(
        outcome(&usdc, "{output: 0, equals: 250000000}"),
        Outcome::Pass
    );
    // Regression: a native unit on a token amount showed both sides in
    // ETH; they are shown in the token's own units.
    let failed = usdc.check("{output: 0, gte: \"1 ether\"}");
    assert_eq!(
        failed.message,
        "evm output '0': expected >= 1000000000000 USDC, got 250 USDC"
    );
    let unknown = Case {
        token: Some(crate::tx::TokenMeta {
            decimals: None,
            symbol: None,
        }),
        ..usdc
    };
    let failed = unknown.check("{output: 0, gte: \"1 ether\"}");
    assert_eq!(
        failed.message,
        "evm output '0': expected >= 1000000000000000000 (1 ether), got 250000000"
    );
}

#[test]
fn gas_lt_reads_the_gas_figure() {
    let case = Case::returned();
    assert_eq!(outcome(&case, "{gas_lt: 30000}"), Outcome::Pass);
    assert_eq!(outcome(&case, "{gas_lt: 21000}"), Outcome::Fail);
    let none = Case::reverted(Revert::Empty);
    assert_eq!(outcome(&none, "{gas_lt: 1}"), Outcome::Error);
}

#[test]
fn failure_messages() {
    let returned = Case::returned();
    let reverted = Case::reverted(Revert::Panic(U256::from(0x12)));
    let lines: Vec<String> = [
        (&returned, "{output: balance, gte: \"1 ether\"}"),
        (&returned, "{output: balance, equals: 7}"),
        (
            &returned,
            "{output: owner, equals: \"0x0000000000000000000000000000000000000001\"}",
        ),
        (&returned, "{output: name, contains: Coin}"),
        (&returned, "{output: delta, gt: 0}"),
        (&returned, "{output: ids, contains: 8}"),
        (&returned, "{output: missing, equals: 1}"),
        (&returned, "{output: name, lt: 1}"),
        (
            &returned,
            "{emits: \"Transfer(address,address,uint256)\", where: {value: 11}}",
        ),
        (&returned, "{emits: \"Approval(address,address,uint256)\"}"),
        (&returned, "{reverts: true}"),
        (&returned, "{reverts_with: InsufficientBalance}"),
        (&returned, "{gas_lt: 21000}"),
        (&reverted, "{reverts: false}"),
        (&reverted, "{reverts_with: InsufficientBalance}"),
        (&reverted, "{output: balance, gte: 1}"),
        (&reverted, "{gas_lt: 1}"),
    ]
    .into_iter()
    .map(|(case, yaml)| {
        let r = case.check(yaml);
        format!("{yaml}\n  {:?}: {}", r.outcome, r.message)
    })
    .collect();
    insta::assert_snapshot!(lines.join("\n"));
}

#[test]
fn only_evm_assertions_are_evaluated() {
    let native = Denomination::default();
    let facts = EvmFacts {
        outputs: None,
        revert: None,
        events: &[],
        gas: None,
        native: &native,
        token: None,
    };
    let list = vec![
        rustman_core::model::Assertion::CloseCode(1000),
        rustman_core::model::Assertion::Evm(Box::new(
            Checked::new(assertion("{reverts: false}")).unwrap(),
        )),
    ];
    let results = evaluate_all(&list, &facts);
    assert_eq!(results.len(), 1);
    assert!(results[0].passed());
}
