//! Multi-provider consistency (chain-exploration "Multi-provider
//! consistency"): head drift over several samples, then the block hash,
//! a balance and the `--call` result at one fixed block, 10 below the
//! lowest head, so reorgs near the head cannot cause false alarms.

use futures_util::future::join_all;
use rustman_report::exploration::Evidence;
use serde_json::{Value, json};

use super::checks::{ProviderRun, pause, quantity};
use super::meter::truncate;
use super::{CheckOutcome, Finding, ProbeOptions, Rule};

/// How far below the lowest head the providers are compared.
pub const COMPARE_MARGIN: u64 = 10;

/// What the comparison found.
#[derive(Debug, Default)]
pub struct Comparison {
    pub common_block: Option<u64>,
    /// Per provider, in order: blocks behind the highest head (the least
    /// over the samples), when every sample answered.
    pub drift: Vec<Option<u64>>,
    /// The last head each provider reported.
    pub heads: Vec<Option<u64>>,
    pub findings: Vec<Finding>,
}

/// Compare `runs` (two or more providers).
pub async fn compare(runs: &mut [ProviderRun<'_>], options: &ProbeOptions) -> Comparison {
    let mut comparison = Comparison {
        drift: vec![None; runs.len()],
        heads: runs.iter().map(|r| r.cx.head).collect(),
        ..Comparison::default()
    };
    drift(runs, options, &mut comparison).await;
    let lowest = comparison.heads.iter().flatten().min().copied();
    let Some(common) = lowest.map(|h| h.saturating_sub(COMPARE_MARGIN)) else {
        return comparison;
    };
    comparison.common_block = Some(common);
    let block = format!("{common:#x}");
    let address = options.address.clone();
    let mut questions = vec![
        Question {
            rule: &BLOCK_HASH,
            what: format!("block hash at block {common}"),
            operation: "block-hash",
            method: "eth_getBlockByNumber",
            params: json!([block, false]),
            pick: |v| v.get("hash").cloned().unwrap_or(Value::Null),
        },
        Question {
            rule: &STATE,
            what: format!("balance of {address} at block {common}"),
            operation: "balance",
            method: "eth_getBalance",
            params: json!([address, block]),
            pick: Value::clone,
        },
    ];
    if let Some(call) = &options.call {
        questions.push(Question {
            rule: &CALL,
            what: format!("eth_call result at block {common}"),
            operation: "call",
            method: "eth_call",
            params: json!([call.tx_object(), block]),
            pick: Value::clone,
        });
    }
    for question in &questions {
        ask(runs, question, &mut comparison.findings).await;
    }
    comparison
}

/// Sample every provider's head `options.timing.drift_samples` times.
async fn drift(runs: &mut [ProviderRun<'_>], options: &ProbeOptions, out: &mut Comparison) {
    let samples = options.timing.drift_samples.max(1);
    let mut lags: Vec<Vec<Option<u64>>> = vec![Vec::new(); runs.len()];
    for sample in 0..samples {
        if sample > 0 && !pause(options.timing.drift_interval, &options.cancel).await {
            break;
        }
        let heads = join_all(
            runs.iter()
                .map(|r| r.meter.call("eth_blockNumber", json!([]))),
        )
        .await;
        let heads: Vec<Option<u64>> = heads
            .iter()
            .map(|h| h.as_ref().ok().and_then(quantity))
            .collect();
        let top = heads.iter().flatten().max().copied();
        for (i, head) in heads.iter().enumerate() {
            if head.is_some() {
                out.heads[i] = *head;
            }
            lags[i].push(top.zip(*head).map(|(top, own)| top - own));
        }
    }
    for (i, run) in runs.iter_mut().enumerate() {
        let lag = lags[i].iter().copied().collect::<Option<Vec<u64>>>();
        let least = lag.as_deref().and_then(|l| l.iter().min().copied());
        out.drift[i] = least;
        let Some(blocks) = least else {
            run.check("head-drift", CheckOutcome::Failed, Some("no head".into()));
            continue;
        };
        if blocks <= options.max_drift {
            run.check(
                "head-drift",
                CheckOutcome::Passed,
                Some(format!("{blocks} blocks")),
            );
            continue;
        }
        let message = format!("head drift {blocks} blocks");
        run.check("head-drift", CheckOutcome::Finding, Some(message.clone()));
        let body = format!(
            "{}: behind the highest head by {} blocks on every sample ({} samples)",
            run.endpoint(),
            blocks,
            lags[i].len()
        );
        out.findings.push(Finding {
            rule: &DRIFT,
            endpoint: run.endpoint().to_owned(),
            operation: "head-drift".to_owned(),
            message,
            evidence: evidence(run.endpoint(), "eth_blockNumber", &json!([]), body),
        });
    }
}

/// One value every provider must agree on.
struct Question {
    rule: &'static Rule,
    what: String,
    operation: &'static str,
    method: &'static str,
    params: Value,
    /// The part of the answer compared.
    pick: fn(&Value) -> Value,
}

/// Ask `question` of every provider; each provider whose answer differs
/// from the first answer is a finding, with both answers as evidence.
async fn ask(runs: &mut [ProviderRun<'_>], question: &Question, findings: &mut Vec<Finding>) {
    let answers = join_all(
        runs.iter()
            .map(|r| r.meter.call(question.method, question.params.clone())),
    )
    .await;
    let picked: Vec<Option<Value>> = answers
        .iter()
        .map(|a| a.as_ref().ok().map(question.pick))
        .collect();
    let Some(reference) = picked.iter().position(Option::is_some) else {
        for (run, answer) in runs.iter_mut().zip(&answers) {
            let reason = answer.as_ref().err().map(|f| f.detail());
            run.check(question.operation, CheckOutcome::Failed, reason);
        }
        return;
    };
    let expected = picked[reference].clone().unwrap_or_default();
    let reference_endpoint = runs[reference].endpoint().to_owned();
    for (i, run) in runs.iter_mut().enumerate() {
        let Some(value) = &picked[i] else {
            let reason = answers[i].as_ref().err().map(|f| f.detail());
            run.check(question.operation, CheckOutcome::Failed, reason);
            continue;
        };
        if *value == expected {
            run.check(question.operation, CheckOutcome::Passed, None);
            continue;
        }
        let message = format!("{} differs from {reference_endpoint}", question.what);
        run.check(
            question.operation,
            CheckOutcome::Finding,
            Some(message.clone()),
        );
        let body = format!(
            "{reference_endpoint}: {expected}\n{}: {value}",
            run.endpoint()
        );
        findings.push(Finding {
            rule: question.rule,
            endpoint: run.endpoint().to_owned(),
            operation: question.operation.to_owned(),
            message,
            evidence: evidence(run.endpoint(), question.method, &question.params, body),
        });
    }
}

/// The longest evidence body a finding keeps, in bytes.
pub const MAX_EVIDENCE_BYTES: usize = 16 * 1024;

/// The evidence of a finding: the call, and the answers in `body` (cut to
/// [`MAX_EVIDENCE_BYTES`]: they are the providers' own text).
pub fn evidence(endpoint: &str, method: &str, params: &Value, body: String) -> Evidence {
    let body = truncate(body, MAX_EVIDENCE_BYTES);
    let request = json!({"jsonrpc": "2.0", "method": method, "params": params});
    Evidence {
        method: "POST".to_owned(),
        url: endpoint.to_owned(),
        request_body: Some(request.to_string()),
        status: 200,
        body,
        ..Evidence::default()
    }
}

pub const DRIFT: Rule = Rule {
    id: "evm-head-drift",
    severity: rustman_report::exploration::Severity::Medium,
    description: "A provider's head is behind the highest provider's by more than --max-drift blocks on every sample.",
    remediation: "Check the provider's sync status, or take it out of rotation until it catches up.",
};

pub const BLOCK_HASH: Rule = Rule {
    id: "evm-block-hash-mismatch",
    severity: rustman_report::exploration::Severity::High,
    description: "Providers return different block hashes for the same block number below the head.",
    remediation: "One provider follows another fork or serves stale data: stop using it for this chain.",
};

pub const STATE: Rule = Rule {
    id: "evm-state-mismatch",
    severity: rustman_report::exploration::Severity::High,
    description: "Providers return different state (a balance) at the same block.",
    remediation: "One provider serves wrong or stale state: stop using it for reads at fixed blocks.",
};

pub const CALL: Rule = Rule {
    id: "evm-call-mismatch",
    severity: rustman_report::exploration::Severity::High,
    description: "Providers return different eth_call results at the same block.",
    remediation: "One provider executes calls on wrong or stale state: stop using it for calls.",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_bodies_are_cut() {
        let body = "z".repeat(MAX_EVIDENCE_BYTES * 4);
        let shown = evidence("http://a", "eth_getCode", &json!([]), body).body;
        assert!(shown.len() < MAX_EVIDENCE_BYTES + 32);
        assert!(shown.ends_with(&format!("({} bytes)", MAX_EVIDENCE_BYTES * 4)));
        let small = evidence("http://a", "eth_getCode", &json!([]), "0x1".into()).body;
        assert_eq!(small, "0x1");
    }
}
