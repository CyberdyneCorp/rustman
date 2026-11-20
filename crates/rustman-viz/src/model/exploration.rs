//! The exploration dashboard's data (spec "Exploration dashboard"): a
//! heatmap of operations by status class, gauges for operation and
//! documented-status coverage, and the findings sorted by severity, then
//! operation, filtered to one operation by clicking its heatmap row.

use std::collections::{BTreeMap, BTreeSet};

use rustman_viz_protocol::{
    ExplorationFinished, ExplorationStarted, Finding, Outcome, ProbeCompleted, RunState, Severity,
};

use super::Phase;
use super::stats::percent;

/// The heatmap columns.
pub const COLUMNS: [&str; 5] = ["2xx", "3xx", "4xx", "5xx", "no response"];

#[derive(Debug, Clone, Default)]
pub struct ExplorationModel {
    pub title: String,
    pub base_url: Option<String>,
    /// Operations in the API description.
    pub operations_total: u64,
    /// Operations that answered at least once.
    pub exercised: BTreeSet<String>,
    /// Probe counts per operation and column, operations in first-seen
    /// order.
    pub rows: Vec<(String, [u64; 5])>,
    pub findings: Vec<Finding>,
    pub statuses_documented: u64,
    pub statuses_seen: u64,
    /// Set by `exploration_finished` (or a report): the final coverage.
    pub final_exercised: Option<u64>,
    pub phase: Phase,
    /// The operation the findings table is filtered to.
    pub filter: Option<String>,
}

impl ExplorationModel {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }

    pub fn started(&mut self, started: &ExplorationStarted) {
        self.operations_total = started.operations;
        self.base_url.clone_from(&started.base_url);
        self.phase = Phase::Running;
    }

    fn row(&mut self, operation: &str) -> &mut [u64; 5] {
        let index = match self.rows.iter().position(|(op, _)| op == operation) {
            Some(index) => index,
            None => {
                self.rows.push((operation.to_owned(), [0; 5]));
                self.rows.len() - 1
            }
        };
        &mut self.rows[index].1
    }

    pub fn probe(&mut self, probe: &ProbeCompleted) {
        let column = match (probe.outcome, probe.status) {
            (_, Some(200..=299)) => Some(0),
            (_, Some(300..=399)) => Some(1),
            (_, Some(400..=499)) => Some(2),
            (_, Some(500..=599)) => Some(3),
            (Outcome::Errored, _) => Some(4),
            _ => None,
        };
        if probe.status.is_some() {
            self.exercised.insert(probe.operation.clone());
        }
        let row = self.row(&probe.operation);
        if let Some(column) = column {
            row[column] += 1;
        }
    }

    /// Count `n` observations in `column` for `operation` (saved reports).
    pub fn observe(&mut self, operation: &str, column: usize, n: u64) {
        self.row(operation)[column.min(4)] += n;
    }

    pub fn finding(&mut self, finding: Finding) {
        self.row(&finding.operation);
        self.findings.push(finding);
    }

    pub fn finished(&mut self, finished: &ExplorationFinished) {
        self.operations_total = finished.operations_total;
        self.final_exercised = Some(finished.operations_exercised);
        self.statuses_documented = finished.status_codes_documented;
        self.statuses_seen = finished.status_codes_seen;
        self.phase = match finished.state {
            RunState::Aborted => Phase::Aborted,
            _ => Phase::Completed,
        };
    }

    /// Operation coverage in percent.
    pub fn operation_coverage(&self) -> f64 {
        let exercised = self.final_exercised.unwrap_or(self.exercised.len() as u64);
        percent(exercised, self.operations_total)
    }

    /// Documented-status coverage in percent (known once finished).
    pub fn status_coverage(&self) -> Option<f64> {
        (self.statuses_documented > 0)
            .then(|| percent(self.statuses_seen, self.statuses_documented))
    }

    /// Heatmap rows, columns and cells.
    pub fn heatmap(&self) -> (Vec<String>, Vec<String>, Vec<Vec<f32>>) {
        let rows = self.rows.iter().map(|(op, _)| op.clone()).collect();
        let columns = COLUMNS.map(str::to_owned).to_vec();
        let cells = self
            .rows
            .iter()
            .map(|(_, counts)| counts.iter().map(|n| *n as f32).collect())
            .collect();
        (rows, columns, cells)
    }

    /// Click on heatmap row `row`: filter the findings to its operation,
    /// or clear the filter when it is already the one shown.
    pub fn click_row(&mut self, row: usize) {
        let Some((operation, _)) = self.rows.get(row) else {
            return;
        };
        self.filter = if self.filter.as_deref() == Some(operation) {
            None
        } else {
            Some(operation.clone())
        };
    }

    /// The findings to list: by severity (critical first), then operation,
    /// then arrival; only the filtered operation's when filtered.
    pub fn findings_table(&self) -> Vec<&Finding> {
        let mut shown: Vec<&Finding> = self
            .findings
            .iter()
            .filter(|f| self.filter.as_ref().is_none_or(|op| &f.operation == op))
            .collect();
        shown.sort_by(|a, b| {
            rank(a.severity)
                .cmp(&rank(b.severity))
                .then_with(|| a.operation.cmp(&b.operation))
        });
        shown
    }

    /// The findings table as text cells.
    pub fn findings_cells(&self) -> (Vec<String>, Vec<Vec<String>>) {
        let columns = ["Severity", "Operation", "Finding", "Detail"]
            .map(str::to_owned)
            .to_vec();
        let rows = self
            .findings_table()
            .into_iter()
            .map(|f| {
                vec![
                    severity_name(f.severity).to_owned(),
                    f.operation.clone(),
                    f.title.clone(),
                    f.detail.clone().unwrap_or_default(),
                ]
            })
            .collect();
        (columns, rows)
    }

    /// The title above the findings table.
    pub fn findings_heading(&self) -> String {
        let total = self.findings.len();
        match &self.filter {
            Some(op) => format!(
                "Findings for {op}: {} of {total} (click the row again to show all)",
                self.findings_table().len()
            ),
            None => format!("Findings: {total}"),
        }
    }

    /// Findings per severity, most severe first.
    pub fn severity_counts(&self) -> BTreeMap<u8, u64> {
        let mut counts = BTreeMap::new();
        for f in &self.findings {
            *counts.entry(rank(f.severity)).or_insert(0) += 1;
        }
        counts
    }
}

/// Sort rank of a severity: critical 0 … info 4, unknown last.
pub fn rank(severity: Severity) -> u8 {
    match severity {
        Severity::Critical => 0,
        Severity::High => 1,
        Severity::Medium => 2,
        Severity::Low => 3,
        Severity::Info => 4,
        Severity::Unknown => 5,
    }
}

pub fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "critical",
        Severity::High => "high",
        Severity::Medium => "medium",
        Severity::Low => "low",
        Severity::Info => "info",
        Severity::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(severity: Severity, operation: &str, title: &str) -> Finding {
        Finding {
            severity,
            operation: operation.into(),
            title: title.into(),
            detail: None,
        }
    }

    fn probe(operation: &str, status: Option<u16>, outcome: Outcome) -> ProbeCompleted {
        ProbeCompleted {
            operation: operation.into(),
            outcome,
            status,
            duration_ms: 5,
        }
    }

    #[test]
    fn coverage_gauges() {
        let mut model = ExplorationModel::new("api.yaml");
        model.started(&ExplorationStarted {
            started_at: "t".into(),
            operations: 24,
            base_url: None,
        });
        for i in 0..18 {
            model.probe(&probe(&format!("GET /r{i}"), Some(200), Outcome::Passed));
        }
        model.probe(&probe("GET /down", None, Outcome::Errored));
        assert_eq!(model.operation_coverage(), 75.0);
        assert_eq!(model.status_coverage(), None);
        model.finished(&ExplorationFinished {
            state: RunState::Completed,
            operations_total: 24,
            operations_exercised: 18,
            status_codes_documented: 40,
            status_codes_seen: 31,
        });
        assert_eq!(model.operation_coverage(), 75.0);
        assert_eq!(model.status_coverage(), Some(77.5));
        let (rows, columns, cells) = model.heatmap();
        assert_eq!(rows.len(), 19);
        assert_eq!(columns.len(), 5);
        assert_eq!(cells[18], [0.0, 0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn clicking_a_row_filters_the_findings() {
        let mut model = ExplorationModel::new("api.yaml");
        model.probe(&probe("POST /users", Some(500), Outcome::Failed));
        model.probe(&probe("GET /orders", Some(200), Outcome::Passed));
        model.finding(finding(Severity::Low, "GET /orders", "slow"));
        model.finding(finding(Severity::High, "POST /users", "500 on empty body"));
        model.finding(finding(Severity::Critical, "GET /orders", "no auth"));
        let order: Vec<_> = model
            .findings_table()
            .iter()
            .map(|f| f.title.as_str())
            .collect();
        assert_eq!(order, ["no auth", "500 on empty body", "slow"]);
        model.click_row(0); // POST /users
        let shown: Vec<_> = model
            .findings_table()
            .iter()
            .map(|f| f.operation.as_str())
            .collect();
        assert_eq!(shown, ["POST /users"]);
        assert!(
            model
                .findings_heading()
                .starts_with("Findings for POST /users: 1 of 3")
        );
        model.click_row(0);
        assert_eq!(model.findings_table().len(), 3);
        model.click_row(99); // nothing there
        assert_eq!(model.filter, None);
    }

    #[test]
    fn a_burst_of_500_findings_is_all_listed() {
        let mut model = ExplorationModel::new("api.yaml");
        for i in 0..500 {
            model.finding(finding(Severity::Medium, &format!("GET /x{}", i % 7), "f"));
        }
        assert_eq!(model.findings_cells().1.len(), 500);
    }
}
