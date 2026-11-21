//! The `subgraph: {max_lag_blocks: N}` assertion (add-evm-toolkit
//! "Subgraph freshness check"): the subgraph's `_meta` block against the
//! chain head of the environment's EVM RPC, read by the GraphQL client
//! into [`SubgraphFacts`].

use super::{AssertionResult, Check, Outcome};
use crate::model::SubgraphAssertion;
use crate::run::SubgraphFacts;

/// The failure without an EVM RPC in the environment.
pub const NO_RPC: &str = "cannot compute lag: no rpc configured";

/// Without facts: the request has no subgraph preset.
pub const NOT_SUBGRAPH: &str = "subgraph: applies to graphql requests with the subgraph preset";

pub(super) fn evaluate(
    assertion: &SubgraphAssertion,
    facts: Option<&SubgraphFacts>,
) -> AssertionResult {
    let max = assertion.max_lag_blocks;
    let check = Check::new("subgraph", format!("subgraph max_lag_blocks {max}"));
    let Some(facts) = facts else {
        return check.error(NOT_SUBGRAPH);
    };
    if !facts.rpc {
        return fail(check, NO_RPC.to_owned());
    }
    if facts.has_indexing_errors {
        return fail(check, "subgraph has indexing errors".to_owned());
    }
    match facts.lag() {
        Some(lag) if lag > max => fail(check, format!("subgraph lag {lag} blocks exceeds {max}")),
        Some(_) => check.pass(),
        None => {
            let why = facts.problem.as_deref().unwrap_or("no head or _meta block");
            fail(check, format!("cannot compute lag: {why}"))
        }
    }
}

fn fail(check: Check, message: String) -> AssertionResult {
    check.result(Outcome::Fail, None, None, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(indexed: u64, head: u64) -> SubgraphFacts {
        SubgraphFacts {
            indexed_block: Some(indexed),
            head: Some(head),
            rpc: true,
            ..SubgraphFacts::default()
        }
    }

    const MAX_50: SubgraphAssertion = SubgraphAssertion { max_lag_blocks: 50 };

    #[test]
    fn a_stale_subgraph_fails_with_its_lag() {
        let result = evaluate(&MAX_50, Some(&facts(1000, 1120)));
        assert_eq!(result.outcome, Outcome::Fail);
        assert_eq!(result.message, "subgraph lag 120 blocks exceeds 50");
        assert_eq!(
            evaluate(&MAX_50, Some(&facts(1000, 1050))).outcome,
            Outcome::Pass
        );
    }

    #[test]
    fn no_rpc_fails_with_the_specified_message() {
        let none = SubgraphFacts {
            indexed_block: Some(1),
            ..SubgraphFacts::default()
        };
        let result = evaluate(&MAX_50, Some(&none));
        assert_eq!(result.outcome, Outcome::Fail);
        assert_eq!(result.message, NO_RPC);
    }

    #[test]
    fn indexing_errors_fail_however_fresh() {
        let broken = SubgraphFacts {
            has_indexing_errors: true,
            ..facts(1000, 1000)
        };
        assert_eq!(
            evaluate(&MAX_50, Some(&broken)).message,
            "subgraph has indexing errors"
        );
    }

    #[test]
    fn unknown_lag_and_no_preset() {
        let unknown = SubgraphFacts {
            rpc: true,
            problem: Some("eth_blockNumber: timeout".into()),
            ..SubgraphFacts::default()
        };
        let result = evaluate(&MAX_50, Some(&unknown));
        assert_eq!(
            result.message,
            "cannot compute lag: eth_blockNumber: timeout"
        );
        let result = evaluate(&MAX_50, None);
        assert_eq!(
            (result.outcome, result.message.as_str()),
            (Outcome::Error, NOT_SUBGRAPH)
        );
    }
}
