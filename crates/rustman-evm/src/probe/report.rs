//! The probe's report: `rustman.report/v1`, `kind: exploration`,
//! `subject: evm-rpc` (chain-exploration "Reports and dashboards"), and
//! the text `rustman evm probe` prints.

use std::fmt::Write as _;
use std::time::Duration;

use rustman_core::run::now_rfc3339;
use rustman_report::SCHEMA;
use rustman_report::exploration::{
    Cell, Coverage, EvmLatency, EvmMethodSupport, EvmProbe, EvmProvider, ExplorationReport, Family,
    KIND_EXPLORATION, OperationInfo, ProbeResult, ProbeTotals, Ratio, ReportFinding, Row,
    SUBJECT_EVM_RPC, Settings, SeverityCounts, StatusCell, Summary,
};
use serde_json::json;

use super::checks::ProviderRun;
use super::compare::{Comparison, evidence};
use super::meter::{millis, percentile};
use super::support::Support;
use super::{Check, CheckOutcome, Finding, ProbeOptions, Rule};

/// When the probe started.
#[derive(Clone, Debug)]
pub struct Clock {
    pub started_at: String,
}

impl Clock {
    pub fn start() -> Self {
        Self {
            started_at: now_rfc3339(),
        }
    }
}

/// A finished probe, before it is a report.
pub struct Probed<'a, 'p> {
    pub runs: &'a [ProviderRun<'p>],
    pub comparison: Comparison,
    pub archive_block: u64,
    pub options: &'a ProbeOptions,
    pub started: Clock,
    pub took: Duration,
}

pub const BUDGET: Rule = Rule {
    id: "evm-budget-exhausted",
    severity: rustman_report::exploration::Severity::Info,
    description: "The request budget ran out before every check of a provider ran.",
    remediation: "Raise --budget, or probe fewer methods with --methods.",
};

pub const RATE_LIMIT: Rule = Rule {
    id: "evm-rate-limit",
    severity: rustman_report::exploration::Severity::Info,
    description: "The provider answered with rate-limit errors during rate discovery.",
    remediation: "Keep clients below the rate found, or move to a plan with a higher limit.",
};

impl Probed<'_, '_> {
    pub fn report(self) -> ExplorationReport {
        let checks: Vec<Check> = self.runs.iter().flat_map(|r| r.checks.clone()).collect();
        let findings = self.findings();
        let probes = probe_results(&checks);
        let report_findings = report_findings(&findings, &checks);
        let endpoints: Vec<String> = self.runs.iter().map(|r| r.endpoint().to_owned()).collect();
        let mut counts = SeverityCounts::default();
        for f in &report_findings {
            counts.add(f.severity);
        }
        ExplorationReport {
            schema: SCHEMA.to_owned(),
            kind: KIND_EXPLORATION.to_owned(),
            rustman_version: rustman_core::VERSION.to_owned(),
            id: report_id(&self.started.started_at),
            started_at: self.started.started_at.clone(),
            finished_at: now_rfc3339(),
            environment: self.options.environment.clone(),
            target: endpoints.join(", "),
            spec: format!("evm probe --methods {}", self.options.methods.spec),
            document_sha256: String::new(),
            seed: 0,
            settings: self.settings(&endpoints),
            summary: Summary {
                probes: totals(&probes),
                findings: counts,
                requests: self
                    .runs
                    .iter()
                    .map(|r| usize::try_from(r.meter.sent()).unwrap_or(usize::MAX))
                    .sum(),
                duration_ms: u64::try_from(self.took.as_millis()).unwrap_or(u64::MAX),
            },
            coverage: self.coverage(),
            findings: report_findings,
            probes,
            left_behind: Vec::new(),
            cleanup: Vec::new(),
            operations: self
                .options
                .methods
                .methods
                .iter()
                .map(|m| OperationInfo {
                    operation: m.name.to_owned(),
                    ..OperationInfo::default()
                })
                .collect(),
            dependencies: Vec::new(),
            subject: Some(SUBJECT_EVM_RPC.to_owned()),
            evm: Some(Box::new(self.evm())),
        }
    }

    /// The comparison's findings, then the budget and rate-limit notes.
    fn findings(&self) -> Vec<Finding> {
        let mut findings = self.comparison.findings.clone();
        for run in self.runs {
            let endpoint = run.endpoint();
            if run.meter.exhausted() {
                let message = format!(
                    "budget of {} requests exhausted; remaining checks skipped (budget)",
                    run.meter.budget()
                );
                findings.push(note(&BUDGET, endpoint, "budget", message));
            }
            if let Some(limit) = run.rate_limit.as_ref().filter(|l| l.limited_at.is_some()) {
                findings.push(note(
                    &RATE_LIMIT,
                    endpoint,
                    "rate-discovery",
                    limit.summary(),
                ));
            }
        }
        findings
    }

    fn settings(&self, endpoints: &[String]) -> Settings {
        let o = self.options;
        Settings {
            probes: vec![Family::EvmRpc],
            operations: o
                .methods
                .methods
                .iter()
                .map(|m| m.name.to_owned())
                .collect(),
            seed: 0,
            fuzz_cases: 0,
            allow_mutations: false,
            allow_destructive: false,
            allow_hosts: endpoints.to_vec(),
            production: None,
            allow_remote_refs: false,
            rate: o.limits.rate.get(),
            concurrency: o.limits.concurrency.get(),
            max_probes: o.budget,
            max_duration_ms: u64::try_from(o.limits.max_duration.as_millis()).unwrap_or(u64::MAX),
            fail_on: o.fail_on,
        }
    }

    /// Methods by providers.
    fn coverage(&self) -> Coverage {
        let mut matrix = Vec::new();
        let (mut cells, mut supported_cells, mut covered) = (0, 0, 0);
        for method in &self.options.methods.methods {
            let statuses: Vec<StatusCell> = self
                .runs
                .iter()
                .map(|run| {
                    let support = support_of(run, method.name);
                    StatusCell {
                        status: run.endpoint().to_owned(),
                        state: cell(support),
                        class: Some(
                            support
                                .unwrap_or(Support::SkippedBudget)
                                .as_str()
                                .to_owned(),
                        ),
                    }
                })
                .collect();
            let observed = statuses
                .iter()
                .filter(|s| s.state == Cell::Observed)
                .count();
            cells += statuses.len();
            supported_cells += observed;
            covered += usize::from(observed > 0);
            matrix.push(Row {
                operation: method.name.to_owned(),
                statuses,
            });
        }
        Coverage {
            operations: Ratio::new(covered, matrix.len()),
            statuses: Ratio::new(supported_cells, cells),
            matrix,
        }
    }

    fn evm(&self) -> EvmProbe {
        let first = self.runs.first();
        let chain = first.map(|r| self.options.registry.get(r.chain_id));
        EvmProbe {
            chain_id: chain.as_ref().map_or(0, |c| c.id),
            chain: chain.map(|c| c.label()).unwrap_or_default(),
            methods: self.options.methods.spec.clone(),
            archive_block: self.archive_block,
            common_block: self.comparison.common_block,
            providers: self
                .runs
                .iter()
                .enumerate()
                .map(|(i, run)| self.provider(i, run))
                .collect(),
        }
    }

    fn provider(&self, index: usize, run: &ProviderRun<'_>) -> EvmProvider {
        let latency = run
            .meter
            .latencies()
            .into_iter()
            .map(|(method, samples)| EvmLatency {
                method,
                samples: samples.len(),
                p50_ms: percentile(&samples, 50),
                p95_ms: percentile(&samples, 95),
                max_ms: samples.iter().max().copied().map_or(0.0, millis),
            })
            .collect();
        EvmProvider {
            endpoint: run.endpoint().to_owned(),
            environment: run.meter.provider.environment.clone(),
            client: run.client.to_string(),
            archive: run.archive.as_str().to_owned(),
            archive_detail: run.archive_detail.clone(),
            methods: run
                .methods
                .iter()
                .map(|(method, support, detail)| EvmMethodSupport {
                    method: (*method).to_owned(),
                    support: support.to_string(),
                    detail: detail.clone(),
                })
                .collect(),
            latency,
            subscriptions: run.subscriptions.clone(),
            rate_limit: run.rate_limit.clone(),
            head: self
                .comparison
                .heads
                .get(index)
                .copied()
                .flatten()
                .or(run.cx.head),
            drift: self.comparison.drift.get(index).copied().flatten(),
            blocks: run.blocks.clone(),
            trace: run.trace.clone(),
            requests: run.meter.sent(),
            budget: run.meter.budget(),
            budget_exhausted: run.meter.exhausted(),
        }
    }
}

fn note(rule: &'static Rule, endpoint: &str, operation: &str, message: String) -> Finding {
    Finding {
        rule,
        endpoint: endpoint.to_owned(),
        operation: operation.to_owned(),
        evidence: evidence(endpoint, "eth_blockNumber", &json!([]), message.clone()),
        message,
    }
}

fn support_of(run: &ProviderRun<'_>, method: &str) -> Option<Support> {
    run.methods
        .iter()
        .find(|(m, _, _)| *m == method)
        .map(|(_, s, _)| *s)
}

fn cell(support: Option<Support>) -> Cell {
    match support {
        Some(Support::Supported) => Cell::Observed,
        Some(Support::SkippedBudget) | None => Cell::Blocked,
        Some(_) => Cell::NotObserved,
    }
}

/// `evm-` and the start time's digits: `evm-20261121T101500`.
fn report_id(started_at: &str) -> String {
    let digits: String = started_at
        .chars()
        .take(19)
        .filter(|c| c.is_ascii_digit() || *c == 'T')
        .collect();
    format!("evm-{digits}")
}

fn probe_results(checks: &[Check]) -> Vec<ProbeResult> {
    checks
        .iter()
        .enumerate()
        .map(|(i, check)| ProbeResult {
            id: format!("p{:04}", i + 1),
            family: Family::EvmRpc,
            operation: check.operation.clone(),
            method: "POST".to_owned(),
            variant: check.endpoint.clone(),
            outcome: outcome(check.outcome).to_owned(),
            reason: match check.outcome {
                CheckOutcome::SkippedBudget => Some("budget".to_owned()),
                _ => check.reason.clone(),
            },
        })
        .collect()
}

fn outcome(outcome: CheckOutcome) -> &'static str {
    match outcome {
        CheckOutcome::Passed => "passed",
        CheckOutcome::Finding => "finding",
        CheckOutcome::SkippedBudget => "skipped",
        CheckOutcome::Failed => "failed",
    }
}

fn totals(probes: &[ProbeResult]) -> ProbeTotals {
    let count = |name: &str| probes.iter().filter(|p| p.outcome == name).count();
    ProbeTotals {
        total: probes.len(),
        passed: count("passed"),
        finding: count("finding"),
        blocked: 0,
        skipped: count("skipped"),
        not_run: 0,
        failed: count("failed"),
    }
}

/// Findings most severe first, numbered `f001`, ...
fn report_findings(findings: &[Finding], checks: &[Check]) -> Vec<ReportFinding> {
    let mut sorted: Vec<&Finding> = findings.iter().collect();
    sorted.sort_by_key(|f| f.rule.severity);
    sorted
        .into_iter()
        .enumerate()
        .map(|(i, f)| ReportFinding {
            id: format!("f{:03}", i + 1),
            rule: f.rule.id.to_owned(),
            severity: f.rule.severity,
            operation: format!("{} {}", f.operation, f.endpoint),
            description: f.rule.description.to_owned(),
            remediation: f.rule.remediation.to_owned(),
            message: f.message.clone(),
            occurrences: 1,
            probes: checks
                .iter()
                .position(|c| c.endpoint == f.endpoint && c.operation == f.operation)
                .map(|i| vec![format!("p{:04}", i + 1)])
                .unwrap_or_default(),
            instance_pointer: None,
            schema_pointer: None,
            evidence: f.evidence.clone(),
            reproduction: None,
        })
        .collect()
}

/// The text `rustman evm probe` prints.
pub fn render_text(report: &ExplorationReport) -> String {
    let mut out = String::new();
    let Some(evm) = &report.evm else {
        return out;
    };
    let _ = writeln!(
        out,
        "evm probe: {}, {} provider(s), archive check at block {}",
        evm.chain,
        evm.providers.len(),
        evm.archive_block
    );
    for provider in &evm.providers {
        provider_text(&mut out, provider);
    }
    if let Some(block) = evm.common_block {
        let _ = writeln!(out, "compared at block {block}");
    }
    let _ = writeln!(out, "findings: {}", report.summary.findings);
    for f in &report.findings {
        let _ = writeln!(
            out,
            "  {}  {:<8}  {}  {}",
            f.id, f.severity, f.rule, f.message
        );
        let _ = writeln!(out, "        {}", f.operation);
    }
    out
}

fn provider_text(out: &mut String, p: &EvmProvider) {
    let _ = writeln!(out, "\nprovider {} ({})", p.endpoint, p.client);
    let _ = writeln!(
        out,
        "  archive: {}   requests: {}/{}{}",
        p.archive,
        p.requests,
        p.budget,
        if p.budget_exhausted {
            " (budget exhausted: remaining checks skipped (budget))"
        } else {
            ""
        }
    );
    if let Some(head) = p.head {
        let drift = p
            .drift
            .map(|d| format!(", drift {d} blocks"))
            .unwrap_or_default();
        let _ = writeln!(out, "  head: {head}{drift}");
    }
    for s in &p.subscriptions {
        let _ = writeln!(out, "  subscription {}: {}", s.kind, s.status);
    }
    if let Some(limit) = &p.rate_limit {
        let _ = writeln!(out, "  {}", limit.summary());
    }
    let _ = writeln!(out, "  {:<40} {:<18} p50/p95/max ms", "method", "support");
    for m in &p.methods {
        let timing = p
            .latency
            .iter()
            .find(|l| l.method == m.method)
            .map(|l| format!("{}/{}/{}", l.p50_ms, l.p95_ms, l.max_ms))
            .unwrap_or_else(|| "-".to_owned());
        let _ = writeln!(out, "  {:<40} {:<18} {timing}", m.method, m.support);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_ids_come_from_the_start_time() {
        assert_eq!(report_id("2026-11-21T10:15:00.123Z"), "evm-20261121T101500");
    }

    #[test]
    fn cells_follow_support() {
        assert_eq!(cell(Some(Support::Supported)), Cell::Observed);
        assert_eq!(cell(Some(Support::Unsupported)), Cell::NotObserved);
        assert_eq!(cell(Some(Support::SkippedBudget)), Cell::Blocked);
        assert_eq!(cell(None), Cell::Blocked);
    }
}
