//! The exploration report (`rustman.report/v1` with `kind: exploration`):
//! the seed and safety settings, probe totals by outcome, the coverage
//! matrix, the findings de-duplicated per rule and operation (with their
//! first evidence and reproduction), and the resources left behind. Every
//! text in it was redacted when the journal was written.

use std::collections::BTreeMap;

use super::coverage::coverage;
use super::detect::Finding;
use super::exec::{Outcome, Status};
use super::journal::{Cleanup, Entry, cleanups, outcomes};
use super::operation_key;
use super::plan::{Plan, Probe};
use super::rules::{SeverityCounts, rule};
use super::runner::live;
use super::session::State;
use crate::openapi::model::ApiModel;

pub use rustman_report::SCHEMA;
pub use rustman_report::exploration::{
    ExplorationReport, KIND_EXPLORATION, OperationInfo, ProbeResult, ProbeTotals, ReportFinding,
    Summary,
};

/// The report of the exploration `state`, from its plan, the model and the
/// journal `entries`.
pub fn build(state: &State, plan: &Plan, model: &ApiModel, entries: &[Entry]) -> ExplorationReport {
    let by_probe: BTreeMap<&str, &Outcome> =
        outcomes(entries).map(|o| (o.probe.as_str(), o)).collect();
    let ordered: Vec<&Outcome> = plan
        .probes
        .iter()
        .filter_map(|p| by_probe.get(p.id.as_str()).copied())
        .collect();
    let findings = findings(&ordered);
    let mut counts = SeverityCounts::default();
    findings.iter().for_each(|f| counts.add(f.severity));
    let probes: Vec<ProbeResult> = plan
        .probes
        .iter()
        .map(|p| result(p, by_probe.get(p.id.as_str()).copied()))
        .collect();
    let cleanup: Vec<Cleanup> = cleanups(entries).cloned().collect();
    let requests = ordered.iter().map(|o| o.sent.len()).sum::<usize>()
        + cleanup.iter().filter(|c| c.status.is_some()).count();
    ExplorationReport {
        schema: SCHEMA.into(),
        kind: KIND_EXPLORATION.into(),
        rustman_version: rustman_core::VERSION.into(),
        id: state.id.clone(),
        started_at: state.started_at.clone(),
        finished_at: state.updated_at.clone(),
        environment: state.environment.clone(),
        target: state.target.clone(),
        spec: state.spec.clone(),
        document_sha256: state.document_sha256.clone(),
        seed: plan.seed,
        settings: state.settings.clone(),
        summary: Summary {
            probes: totals(&probes),
            findings: counts,
            requests,
            duration_ms: state.executed_ms,
        },
        coverage: coverage(model, plan, &ordered),
        findings,
        probes,
        left_behind: live(entries),
        cleanup,
        operations: operations(model),
    }
}

/// The findings of `outcomes` (in plan order), one per rule and operation,
/// most severe first.
fn findings(outcomes: &[&Outcome]) -> Vec<ReportFinding> {
    let mut found: Vec<ReportFinding> = Vec::new();
    for (outcome, finding) in outcomes
        .iter()
        .flat_map(|o| o.findings.iter().map(move |f| (*o, f)))
    {
        let same =
            |r: &&mut ReportFinding| r.rule == finding.rule && r.operation == finding.operation;
        match found.iter_mut().find(same) {
            Some(existing) => {
                existing.occurrences += 1;
                if existing.probes.last() != Some(&outcome.probe) {
                    existing.probes.push(outcome.probe.clone());
                }
            }
            None => found.push(first(finding)),
        }
    }
    found.sort_by_key(|f| f.severity);
    for (i, f) in found.iter_mut().enumerate() {
        f.id = format!("f{:03}", i + 1);
    }
    found
}

/// The documented operations of `model`, as reports group and locate them.
fn operations(model: &ApiModel) -> Vec<OperationInfo> {
    model
        .operations
        .iter()
        .map(|op| OperationInfo {
            operation: operation_key(op),
            operation_id: op.operation_id.clone(),
            tags: op.tags.clone(),
        })
        .collect()
}

fn first(finding: &Finding) -> ReportFinding {
    let registered = rule(&finding.rule);
    ReportFinding {
        id: String::new(),
        rule: finding.rule.clone(),
        severity: finding.severity,
        operation: finding.operation.clone(),
        description: registered.map_or_else(String::new, |r| r.description.to_owned()),
        remediation: registered.map_or_else(String::new, |r| r.remediation.to_owned()),
        message: finding.message.clone(),
        occurrences: 1,
        probes: vec![finding.probe.clone()],
        instance_pointer: finding.instance_pointer.clone(),
        schema_pointer: finding.schema_pointer.clone(),
        evidence: finding.evidence.clone(),
        reproduction: finding.reproduction.clone(),
    }
}

fn result(probe: &Probe, outcome: Option<&Outcome>) -> ProbeResult {
    let (label, reason) = match outcome {
        None => ("not run", Some("interrupted".to_owned())),
        Some(o) => classify(o),
    };
    ProbeResult {
        id: probe.id.clone(),
        family: probe.family,
        operation: probe.operation.clone(),
        method: probe.method.clone(),
        variant: probe.variant.clone(),
        outcome: label.to_owned(),
        reason,
    }
}

fn classify(outcome: &Outcome) -> (&'static str, Option<String>) {
    let found = !outcome.findings.is_empty();
    match &outcome.status {
        Status::Done if found => ("finding", None),
        Status::Done => ("passed", None),
        Status::Failed(reason) if found => ("finding", Some(reason.clone())),
        Status::Failed(reason) => ("failed", Some(reason.clone())),
        Status::Blocked(reason) => ("blocked", Some(reason.clone())),
        Status::Skipped(reason) => ("skipped", Some(reason.clone())),
        Status::NotRun(reason) => ("not run", Some(reason.clone())),
    }
}

fn totals(probes: &[ProbeResult]) -> ProbeTotals {
    let mut totals = ProbeTotals {
        total: probes.len(),
        ..ProbeTotals::default()
    };
    for p in probes {
        let slot = match p.outcome.as_str() {
            "passed" => &mut totals.passed,
            "finding" => &mut totals.finding,
            "blocked" => &mut totals.blocked,
            "skipped" => &mut totals.skipped,
            "failed" => &mut totals.failed,
            _ => &mut totals.not_run,
        };
        *slot += 1;
    }
    totals
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explore::detect::Evidence;
    use crate::explore::exec::Sent;
    use crate::explore::rules::Severity;

    fn finding(rule: &str, severity: Severity, probe: &str) -> Finding {
        Finding {
            rule: rule.into(),
            severity,
            operation: "POST /users".into(),
            probe: probe.into(),
            message: format!("{rule} in {probe}"),
            instance_pointer: None,
            schema_pointer: None,
            evidence: Evidence {
                status: 500,
                ..Evidence::default()
            },
            reproduction: None,
        }
    }

    #[test]
    fn duplicate_findings_collapse_with_their_first_evidence() {
        let outcomes: Vec<Outcome> = (1..=12)
            .map(|i| {
                let id = format!("p{i:04}");
                let mut o = Outcome::new(&id, Status::Done);
                o.findings.push(finding(
                    "server-error-on-invalid-input",
                    Severity::High,
                    &id,
                ));
                if i == 3 {
                    o.findings
                        .push(finding("auth-not-enforced", Severity::Critical, &id));
                }
                o
            })
            .collect();
        let refs: Vec<&Outcome> = outcomes.iter().collect();
        let found = findings(&refs);
        assert_eq!(found.len(), 2);
        assert_eq!(
            (found[0].id.as_str(), found[0].rule.as_str()),
            ("f001", "auth-not-enforced")
        );
        let fuzz = &found[1];
        assert_eq!(fuzz.occurrences, 12);
        assert_eq!(fuzz.message, "server-error-on-invalid-input in p0001");
        assert_eq!(fuzz.probes.len(), 12);
    }

    #[test]
    fn outcomes_are_classified() {
        let mut found = Outcome::new("p", Status::Failed("x".into()));
        found
            .findings
            .push(finding("server-error", Severity::High, "p"));
        found.sent.push(Sent {
            step: "probe".into(),
            operation: None,
            method: "GET".into(),
            url: String::new(),
            status: 500,
        });
        assert_eq!(classify(&found).0, "finding");
        assert_eq!(classify(&Outcome::new("p", Status::Done)).0, "passed");
        assert_eq!(
            classify(&Outcome::new("p", Status::NotRun("budget".into()))),
            ("not run", Some("budget".into()))
        );
        assert_eq!(
            classify(&Outcome::new("p", Status::Failed("e".into()))).0,
            "failed"
        );
    }
}
