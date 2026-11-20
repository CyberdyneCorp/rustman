//! `report diff`: what changed between two reports of the same kind.
//! Findings match by rule id and operation and are `new`, `fixed` or
//! `unchanged`; coverage deltas compare the two explorations; assertions
//! that changed state compare two runs request by request.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::exploration::{ExplorationReport, Ratio, ReportFinding, Severity};
use crate::report::Report;
use crate::run::RunReport;
use crate::stress::StressReport;

/// One finding on either side, as a diff lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiffFinding {
    pub rule: String,
    pub operation: String,
    pub severity: Severity,
    /// Its id in the report it comes from (the new one when in both).
    pub id: String,
    pub message: String,
}

/// The change of one coverage ratio.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RatioDelta {
    pub old: Ratio,
    pub new: Ratio,
    /// Percentage points, new minus old.
    pub delta: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct CoverageDelta {
    pub operations: RatioDelta,
    pub statuses: RatioDelta,
}

/// One assertion whose outcome changed between two runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AssertionChange {
    /// The request id, with `[iteration]` past the first.
    pub request: String,
    pub assertion: String,
    /// `pass`, `fail`, `error`, or `absent` when only one run has it.
    pub old: String,
    pub new: String,
}

/// The whole difference.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Diff {
    pub kind: &'static str,
    pub new: Vec<DiffFinding>,
    pub fixed: Vec<DiffFinding>,
    pub unchanged: Vec<DiffFinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageDelta>,
    pub assertions: Vec<AssertionChange>,
}

/// Two reports of different kinds, or of a kind `report diff` does not
/// compare (contract reports), cannot be compared.
#[derive(Debug, thiserror::Error)]
#[error("{}", mismatch(self.old, self.new))]
pub struct KindMismatch {
    pub old: &'static str,
    pub new: &'static str,
}

fn mismatch(old: &str, new: &str) -> String {
    if old == new {
        format!(
            "cannot compare {old} reports (report diff compares runs, explorations and stress runs)"
        )
    } else {
        format!(
            "cannot compare {} report with {} report",
            article(old),
            article(new)
        )
    }
}

/// `kind` with its indefinite article: `a run`, `an exploration`.
fn article(kind: &str) -> String {
    let an = kind.starts_with(['a', 'e', 'i', 'o', 'u']);
    format!("{} {kind}", if an { "an" } else { "a" })
}

impl Diff {
    /// New findings at or above `threshold`: `report diff` exits 1 when
    /// there is any.
    pub fn regressions(&self, threshold: Severity) -> usize {
        self.new.iter().filter(|f| f.severity <= threshold).count()
    }

    /// As pretty-printed JSON, ending in a newline.
    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("diffs serialize");
        text.push('\n');
        text
    }

    /// As Markdown.
    pub fn to_markdown(&self) -> String {
        let mut out = format!(
            "# Report diff ({})\n\n{} new, {} fixed, {} unchanged findings.\n",
            self.kind,
            self.new.len(),
            self.fixed.len(),
            self.unchanged.len()
        );
        for (title, list) in [
            ("New", &self.new),
            ("Fixed", &self.fixed),
            ("Unchanged", &self.unchanged),
        ] {
            findings_section(&mut out, title, list);
        }
        if let Some(c) = &self.coverage {
            out.push_str("\n## Coverage\n\n| | Old | New | Change |\n|---|---:|---:|---:|\n");
            ratio_row(&mut out, "Operations", &c.operations);
            ratio_row(&mut out, "Statuses", &c.statuses);
        }
        if !self.assertions.is_empty() {
            out.push_str(
                "\n## Assertions\n\n| Request | Assertion | Old | New |\n|---|---|---|---|\n",
            );
            for a in &self.assertions {
                out.push_str(&format!(
                    "| `{}` | {} | {} | {} |\n",
                    a.request, a.assertion, a.old, a.new
                ));
            }
        }
        out
    }
}

fn findings_section(out: &mut String, title: &str, list: &[DiffFinding]) {
    if list.is_empty() {
        return;
    }
    out.push_str(&format!(
        "\n## {title}\n\n| Severity | Rule | Operation | ID |\n|---|---|---|---|\n"
    ));
    for f in list {
        out.push_str(&format!(
            "| {} | `{}` | `{}` | {} |\n",
            f.severity, f.rule, f.operation, f.id
        ));
    }
}

fn ratio_row(out: &mut String, name: &str, d: &RatioDelta) {
    out.push_str(&format!(
        "| {name} | {}/{} ({}%) | {}/{} ({}%) | {:+.1} |\n",
        d.old.covered,
        d.old.total,
        d.old.percent,
        d.new.covered,
        d.new.total,
        d.new.percent,
        d.delta
    ));
}

/// The difference from `old` to `new`.
pub fn diff(old: &Report, new: &Report) -> Result<Diff, KindMismatch> {
    match (old, new) {
        (Report::Exploration(o), Report::Exploration(n)) => Ok(explorations(o, n)),
        (Report::Run(o), Report::Run(n)) => Ok(runs(o, n)),
        (Report::Stress(o), Report::Stress(n)) => Ok(stresses(o, n)),
        _ => Err(KindMismatch {
            old: old.kind(),
            new: new.kind(),
        }),
    }
}

type Key<'a> = (&'a str, &'a str);

fn by_key(findings: &[ReportFinding]) -> BTreeMap<Key<'_>, &ReportFinding> {
    findings
        .iter()
        .map(|f| ((f.rule.as_str(), f.operation.as_str()), f))
        .collect()
}

fn explorations(old: &ExplorationReport, new: &ExplorationReport) -> Diff {
    Diff {
        coverage: Some(CoverageDelta {
            operations: delta(old.coverage.operations, new.coverage.operations),
            statuses: delta(old.coverage.statuses, new.coverage.statuses),
        }),
        ..findings("exploration", &old.findings, &new.findings)
    }
}

/// Stress findings match by rule and operation (the request file) like
/// exploration findings; there is no coverage.
fn stresses(old: &StressReport, new: &StressReport) -> Diff {
    let base = |r: &StressReport| -> Vec<ReportFinding> {
        r.findings.iter().map(|f| f.finding.clone()).collect()
    };
    findings("stress", &base(old), &base(new))
}

/// New, fixed and unchanged findings between `old` and `new`.
fn findings(kind: &'static str, old: &[ReportFinding], new: &[ReportFinding]) -> Diff {
    let (before, after) = (by_key(old), by_key(new));
    let pick = |from: &BTreeMap<Key, &ReportFinding>,
                other: &BTreeMap<Key, &ReportFinding>,
                present: bool| {
        let mut list: Vec<DiffFinding> = from
            .iter()
            .filter(|(k, _)| other.contains_key(*k) == present)
            .map(|(_, f)| entry(f))
            .collect();
        list.sort_by(|a, b| {
            (a.severity, &a.rule, &a.operation).cmp(&(b.severity, &b.rule, &b.operation))
        });
        list
    };
    Diff {
        kind,
        new: pick(&after, &before, false),
        fixed: pick(&before, &after, false),
        unchanged: pick(&after, &before, true),
        coverage: None,
        assertions: Vec::new(),
    }
}

fn entry(f: &ReportFinding) -> DiffFinding {
    DiffFinding {
        rule: f.rule.clone(),
        operation: f.operation.clone(),
        severity: f.severity,
        id: f.id.clone(),
        message: f.message.clone(),
    }
}

fn delta(old: Ratio, new: Ratio) -> RatioDelta {
    RatioDelta {
        old,
        new,
        delta: ((new.percent - old.percent) * 10.0).round() / 10.0,
    }
}

/// `request [iteration]` -> assertion label -> outcome.
type Outcomes = BTreeMap<String, BTreeMap<String, String>>;

fn outcomes(run: &RunReport) -> Outcomes {
    let mut map = Outcomes::new();
    for q in &run.requests {
        let name = if q.iteration > 1 {
            format!("{} [{}]", q.id, q.iteration)
        } else {
            q.id.clone()
        };
        let entry = map.entry(name).or_default();
        for a in &q.assertions {
            entry.insert(a.label.clone(), a.outcome.clone());
        }
    }
    map
}

fn runs(old: &RunReport, new: &RunReport) -> Diff {
    let (before, after) = (outcomes(old), outcomes(new));
    let empty = BTreeMap::new();
    let mut assertions = Vec::new();
    let requests: std::collections::BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    for request in requests {
        let (o, n) = (
            before.get(request).unwrap_or(&empty),
            after.get(request).unwrap_or(&empty),
        );
        let labels: std::collections::BTreeSet<&String> = o.keys().chain(n.keys()).collect();
        for label in labels {
            let state = |m: &BTreeMap<String, String>| {
                m.get(label).cloned().unwrap_or_else(|| "absent".to_owned())
            };
            let (old_state, new_state) = (state(o), state(n));
            if old_state != new_state {
                assertions.push(AssertionChange {
                    request: request.clone(),
                    assertion: label.clone(),
                    old: old_state,
                    new: new_state,
                });
            }
        }
    }
    Diff {
        kind: "run",
        new: Vec::new(),
        fixed: Vec::new(),
        unchanged: Vec::new(),
        coverage: None,
        assertions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_mismatches_read_with_the_right_article() {
        let mismatch = |old, new| KindMismatch { old, new }.to_string();
        assert_eq!(
            mismatch("exploration", "run"),
            "cannot compare an exploration report with a run report"
        );
        assert_eq!(
            mismatch("run", "exploration"),
            "cannot compare a run report with an exploration report"
        );
        assert_eq!(
            mismatch("contract", "contract"),
            "cannot compare contract reports (report diff compares runs, explorations and stress runs)"
        );
    }
}
