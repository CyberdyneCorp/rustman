//! Saved reports (`load_report`, `rustman viz <REPORT>`): `rustman.report/v1`
//! run, exploration and bench reports, read loosely from JSON so that fields added
//! by later versions are ignored. The companion depends on no other
//! Rustman crate than the protocol (spec "Read-only companion").

use rustman_viz_protocol::{
    BenchFinished, BenchThreshold, BenchTick, ExplorationFinished, ExplorationStarted, Finding,
    Outcome, RunState, Severity,
};
use serde_json::Value;

use crate::model::Phase;
use crate::model::bench::BenchModel;
use crate::model::exploration::ExplorationModel;
use crate::model::graph::GraphModel;
use crate::model::run::{Exchange, RunModel};

/// A report, as the views read it.
#[derive(Debug, Clone)]
pub enum Loaded {
    Run(RunModel),
    Exploration {
        dashboard: ExplorationModel,
        graph: GraphModel,
    },
    Bench(BenchModel),
}

/// Read the report at `path`.
pub fn load(path: &str) -> Result<Loaded, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    parse(&text, path)
}

/// Parse a report; `title` names it in the window.
pub fn parse(text: &str, title: &str) -> Result<Loaded, String> {
    let report: Value =
        serde_json::from_str(text).map_err(|_| format!("{title} is not a Rustman report"))?;
    let schema = report["schema"].as_str().unwrap_or_default();
    if !schema.starts_with("rustman.report/v1") {
        return Err(format!("{title} is not a rustman.report/v1 report"));
    }
    match report["kind"].as_str() {
        Some("run") => Ok(Loaded::Run(run(&report, title))),
        Some("exploration") => Ok(exploration(&report, title)),
        Some("bench") => Ok(Loaded::Bench(bench(&report, title))),
        other => Err(format!(
            "{title}: unsupported report kind {}",
            other.unwrap_or("(none)")
        )),
    }
}

fn str_of(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn run(report: &Value, title: &str) -> RunModel {
    let mut model = RunModel::new(title);
    model.environment = report["environment"].as_str().map(str::to_owned);
    for request in report["requests"].as_array().into_iter().flatten() {
        let outcome = match (request["status"].as_str(), request["skip_reason"].as_str()) {
            (Some("passed"), _) => Outcome::Passed,
            (Some("failed"), _) => Outcome::Failed,
            (Some("errored"), _) => Outcome::Errored,
            (Some("skipped"), Some("interrupted")) => Outcome::Cancelled,
            _ => Outcome::Skipped,
        };
        let assertions = request["assertions"].as_array();
        let count = |pass: bool| {
            assertions
                .into_iter()
                .flatten()
                .filter(|a| (a["outcome"].as_str() == Some("pass")) == pass)
                .count() as u32
        };
        model.exchange(Exchange {
            request: str_of(&request["id"]),
            outcome,
            status: request["exchange"]["response"]["status"]
                .as_u64()
                .and_then(|s| u16::try_from(s).ok()),
            duration_ms: request["duration_ms"].as_f64().unwrap_or(0.0),
            assertions_passed: count(true),
            assertions_failed: count(false),
        });
    }
    model.phase = if report["summary"]["interrupted"].as_bool() == Some(true) {
        Phase::Aborted
    } else {
        Phase::Completed
    };
    model
}

fn severity(value: &Value) -> Severity {
    serde_json::from_value(value.clone()).unwrap_or(Severity::Unknown)
}

fn exploration(report: &Value, title: &str) -> Loaded {
    let mut dashboard = ExplorationModel::new(title);
    let coverage = &report["coverage"];
    dashboard.started(&ExplorationStarted {
        started_at: str_of(&report["started_at"]),
        operations: coverage["operations"]["total"].as_u64().unwrap_or(0),
        base_url: report["target"].as_str().map(str::to_owned),
    });
    // A report keeps no per-probe status: the heatmap counts the documented
    // statuses observed per class, and the status of each finding's
    // evidence.
    for row in coverage["matrix"].as_array().into_iter().flatten() {
        let operation = str_of(&row["operation"]);
        dashboard.observe(&operation, 0, 0);
        for cell in row["statuses"].as_array().into_iter().flatten() {
            if cell["state"].as_str() != Some("observed") {
                continue;
            }
            if let Some(column) = status_column(cell["status"].as_str().unwrap_or_default()) {
                dashboard.observe(&operation, column, 1);
            }
        }
    }
    let findings: Vec<Finding> = report["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| Finding {
            severity: severity(&f["severity"]),
            operation: str_of(&f["operation"]),
            title: str_of(&f["message"]),
            detail: Some(format!(
                "{} ({}×)",
                f["rule"].as_str().unwrap_or_default(),
                f["occurrences"].as_u64().unwrap_or(1)
            )),
        })
        .collect();
    for finding in &findings {
        dashboard.finding(finding.clone());
    }
    dashboard.finished(&ExplorationFinished {
        state: RunState::Completed,
        operations_total: coverage["operations"]["total"].as_u64().unwrap_or(0),
        operations_exercised: coverage["operations"]["covered"].as_u64().unwrap_or(0),
        status_codes_documented: coverage["statuses"]["total"].as_u64().unwrap_or(0),
        status_codes_seen: coverage["statuses"]["covered"].as_u64().unwrap_or(0),
    });
    let operations: Vec<String> = report["operations"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| str_of(&o["operation"]))
        .collect();
    let dependencies: Vec<(String, String, String)> = report["dependencies"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| {
            (
                str_of(&d["producer"]),
                str_of(&d["consumer"]),
                str_of(&d["parameter"]),
            )
        })
        .collect();
    let graph = GraphModel::new(title, &operations, &dependencies, findings);
    Loaded::Exploration { dashboard, graph }
}

fn f64_of(value: &Value) -> f64 {
    value.as_f64().unwrap_or(0.0)
}

fn u64_of(value: &Value) -> u64 {
    value.as_u64().unwrap_or(0)
}

/// A bench report (`rustman-report`'s `BenchReport`): its per-second
/// series become the ticks, its totals and thresholds the end.
fn bench(report: &Value, title: &str) -> BenchModel {
    let mut model = BenchModel::new(title);
    model.target = report["target"].as_str().map(str::to_owned);
    model.environment = report["environment"].as_str().map(str::to_owned);
    model.profile = profile(&report["profile"]);
    for second in report["series"].as_array().into_iter().flatten() {
        let n = u32::try_from(u64_of(&second["second"])).unwrap_or(u32::MAX);
        model.tick(&BenchTick {
            second: n,
            elapsed_ms: u64::from(n) * 1000,
            remaining_ms: None,
            active_vus: u32::try_from(u64_of(&second["active_vus"])).unwrap_or(u32::MAX),
            requests: u64_of(&second["requests"]),
            errors: u64_of(&second["errors"]),
            rps: f64_of(&second["rps"]),
            p50_ms: f64_of(&second["p50_ms"]),
            p95_ms: f64_of(&second["p95_ms"]),
            p99_ms: f64_of(&second["p99_ms"]),
        });
    }
    let overall = &report["overall"];
    let latency = &overall["latency"];
    let errors = overall["errors"]
        .as_object()
        .map_or(0, |classes| classes.values().map(u64_of).sum());
    let aborted = report["aborted"].as_str().map(str::to_owned);
    model.finished(&BenchFinished {
        state: if aborted.is_some() {
            RunState::Aborted
        } else {
            RunState::Completed
        },
        duration_ms: f64_of(&report["totals"]["duration_ms"]).max(0.0).round() as u64,
        iterations: u64_of(&report["totals"]["iterations"]),
        requests: u64_of(&overall["requests"]),
        errors,
        rps: f64_of(&report["totals"]["rps"]),
        p50_ms: f64_of(&latency["p50_ms"]),
        p95_ms: f64_of(&latency["p95_ms"]),
        p99_ms: f64_of(&latency["p99_ms"]),
        thresholds: report["thresholds"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| BenchThreshold {
                expression: str_of(&t["expression"]),
                passed: t["passed"].as_bool().unwrap_or(false),
                actual: str_of(&t["actual"]),
            })
            .collect(),
        aborted,
    });
    model
}

/// The load profile in words: `constant-vus · 5 VUs · 3 s`.
fn profile(profile: &Value) -> Option<String> {
    let executor = profile["executor"].as_str()?;
    let mut parts = vec![executor.to_owned()];
    if let Some(vus) = profile["vus"].as_u64() {
        parts.push(format!("{vus} VUs"));
    }
    if let Some(rate) = profile["rate"].as_u64() {
        parts.push(format!("{rate} iterations/s"));
    }
    if let Some(ms) = profile["duration_ms"].as_u64() {
        parts.push(format!("{} s", ms / 1000));
    }
    if let Some(iterations) = profile["iterations"].as_u64() {
        parts.push(format!("{iterations} iterations"));
    }
    Some(parts.join(" · "))
}

/// The heatmap column of a documented status: `200`, `4XX`, `default`.
fn status_column(status: &str) -> Option<usize> {
    match status.as_bytes().first()? {
        b'2' => Some(0),
        b'3' => Some(1),
        b'4' => Some(2),
        b'5' => Some(3),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN: &str = include_str!("../tests/fixtures/run-report.json");
    const EXPLORATION: &str = include_str!("../tests/fixtures/exploration-report.json");
    const BENCH: &str = include_str!("../tests/fixtures/bench-report.json");

    #[test]
    fn a_run_report_fills_the_run_dashboard() {
        let Loaded::Run(run) = parse(RUN, "run.json").unwrap() else {
            panic!("not a run");
        };
        assert_eq!(run.outcome_counts(), [180, 0, 15, 0, 5, 0]);
        assert_eq!(run.tile_texts()[0], "200");
        assert_eq!(run.phase, Phase::Completed);
        assert_eq!(run.environment.as_deref(), Some("staging"));
    }

    #[test]
    fn an_exploration_report_fills_the_dashboard_and_the_graph() {
        let Loaded::Exploration { dashboard, graph } =
            parse(EXPLORATION, "exploration.json").unwrap()
        else {
            panic!("not an exploration");
        };
        assert_eq!(dashboard.operation_coverage(), 75.0);
        assert_eq!(dashboard.operations_total, 24);
        assert_eq!(dashboard.findings.len(), 3);
        assert_eq!(dashboard.findings_table()[0].severity, Severity::Critical);
        let producer = graph
            .nodes
            .iter()
            .position(|n| n.operation == "POST /users")
            .unwrap();
        let consumer = graph
            .nodes
            .iter()
            .position(|n| n.operation == "GET /users/{id}")
            .unwrap();
        assert!(
            graph
                .links
                .iter()
                .any(|l| l.producer == producer && l.consumer == consumer),
            "{:?}",
            graph.links
        );
    }

    #[test]
    fn a_bench_report_fills_the_bench_dashboard() {
        let Loaded::Bench(bench) = parse(BENCH, "bench.json").unwrap() else {
            panic!("not a bench");
        };
        assert_eq!(bench.phase, Phase::Completed);
        assert_eq!(bench.seconds.len(), 3);
        assert_eq!(bench.counts(), (120, 3));
        assert_eq!(
            bench.subtitle(),
            "https://staging.example.com · environment staging · constant-vus · 5 VUs · 3 s · 3 s elapsed"
        );
        assert_eq!(bench.tile_texts(), ["120", "39.8", "412 ms", "2.5%", "5"]);
        assert_eq!(bench.thresholds_heading(), "Thresholds: 1 of 2 failed");
        let (_, rows) = bench.threshold_table();
        assert_eq!(rows[0], ["p95<300ms", "✗ failed", "412ms"]);
    }

    #[test]
    fn an_aborted_bench_report_says_why() {
        let mut report: Value = serde_json::from_str(BENCH).unwrap();
        report["aborted"] = "interrupted".into();
        let Loaded::Bench(bench) = parse(&report.to_string(), "b.json").unwrap() else {
            panic!("not a bench");
        };
        assert_eq!(bench.state_text(), "Aborted (interrupted)");
    }

    #[test]
    fn other_files_are_refused() {
        for (text, wanted) in [
            ("just notes", "is not a Rustman report"),
            (r#"{"a":1}"#, "is not a rustman.report/v1 report"),
            (
                r#"{"schema":"rustman.report/v1","kind":"contract"}"#,
                "unsupported report kind contract",
            ),
        ] {
            let err = parse(text, "f.json").unwrap_err();
            assert!(err.contains(wanted), "{err}");
        }
    }
}
