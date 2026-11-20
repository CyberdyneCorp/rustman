//! The run dashboard's data (spec "Run dashboard"): KPI tiles with their
//! history, outcomes by class, latency over time, a latency histogram and
//! one row per request file. Fed by live events or by a saved run report.

use std::collections::BTreeMap;

use rustman_viz_protocol::{ExchangeCompleted, Outcome, RunFinished, RunStarted, RunState};

use super::Phase;
use super::stats::{format_ms, format_percent, percent, percentile, text_sparkline};

/// The outcome classes of the donut, in display order.
pub const CLASSES: [&str; 6] = ["2xx", "3xx", "4xx", "5xx", "transport error", "cancelled"];

/// Histogram bucket upper bounds in milliseconds; the last bucket is open.
pub const BUCKETS: [f64; 9] = [
    10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0,
];

/// How many points each KPI sparkline keeps.
const HISTORY: usize = 120;

/// One finished request, as the dashboard counts it.
#[derive(Debug, Clone, PartialEq)]
pub struct Exchange {
    pub request: String,
    pub outcome: Outcome,
    pub status: Option<u16>,
    pub duration_ms: f64,
    pub assertions_passed: u32,
    pub assertions_failed: u32,
}

impl From<&ExchangeCompleted> for Exchange {
    fn from(e: &ExchangeCompleted) -> Self {
        Self {
            request: e.request.clone(),
            outcome: e.outcome,
            status: e.status,
            duration_ms: e.duration_ms as f64,
            assertions_passed: e.assertions.passed,
            assertions_failed: e.assertions.failed,
        }
    }
}

/// The five KPI tiles.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Kpis {
    pub requests: u64,
    /// Assertions passed, in percent (requests passed when no assertion
    /// ran).
    pub pass_rate: f64,
    pub p50: f64,
    pub p95: f64,
    /// Requests that got no response (transport errors).
    pub errors: u64,
}

/// One row of the per-request table.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestRow {
    pub request: String,
    pub count: u64,
    pub pass_rate: f64,
    pub p50: f64,
    pub p95: f64,
    /// Its latencies, oldest first.
    pub latencies: Vec<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct RunModel {
    pub title: String,
    pub environment: Option<String>,
    /// How many exchanges the run planned, when known.
    pub planned: Option<u64>,
    pub exchanges: Vec<Exchange>,
    pub phase: Phase,
    /// KPI values after each exchange, for the tile sparklines.
    pub history: Vec<Kpis>,
}

impl RunModel {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }

    pub fn started(&mut self, started: &RunStarted) {
        self.environment.clone_from(&started.environment);
        self.planned = started.total;
        self.phase = Phase::Running;
    }

    pub fn exchange(&mut self, exchange: Exchange) {
        self.exchanges.push(exchange);
        let kpis = self.kpis();
        self.history.push(kpis);
        if self.history.len() > HISTORY {
            self.history.remove(0);
        }
    }

    pub fn finished(&mut self, finished: &RunFinished) {
        self.phase = match finished.state {
            RunState::Aborted => Phase::Aborted,
            _ => Phase::Completed,
        };
    }

    /// The exchanges that were sent (skipped ones are left out).
    fn sent(&self) -> impl Iterator<Item = &Exchange> {
        self.exchanges
            .iter()
            .filter(|e| e.outcome != Outcome::Skipped)
    }

    pub fn kpis(&self) -> Kpis {
        let latencies: Vec<f64> = self
            .sent()
            .filter(|e| e.outcome != Outcome::Cancelled)
            .map(|e| e.duration_ms)
            .collect();
        let (passed, failed) = self.sent().fold((0u64, 0u64), |(p, f), e| {
            (
                p + u64::from(e.assertions_passed),
                f + u64::from(e.assertions_failed),
            )
        });
        let pass_rate = if passed + failed > 0 {
            percent(passed, passed + failed)
        } else {
            let ok = self.sent().filter(|e| e.outcome == Outcome::Passed).count() as u64;
            percent(ok, self.sent().count() as u64)
        };
        Kpis {
            requests: self.sent().count() as u64,
            pass_rate,
            p50: percentile(&latencies, 50.0).unwrap_or(0.0),
            p95: percentile(&latencies, 95.0).unwrap_or(0.0),
            errors: self
                .sent()
                .filter(|e| e.outcome == Outcome::Errored)
                .count() as u64,
        }
    }

    /// The tile texts: requests, pass rate, p50, p95, errors.
    pub fn tile_texts(&self) -> [String; 5] {
        let k = self.kpis();
        let requests = match self.planned {
            Some(planned) if self.phase == Phase::Running => format!("{} / {planned}", k.requests),
            _ => k.requests.to_string(),
        };
        [
            requests,
            format_percent(k.pass_rate),
            format_ms(k.p50),
            format_ms(k.p95),
            k.errors.to_string(),
        ]
    }

    /// The sparkline values of each tile, oldest first.
    pub fn tile_history(&self) -> [Vec<f32>; 5] {
        let series = |f: fn(&Kpis) -> f64| self.history.iter().map(|k| f(k) as f32).collect();
        [
            series(|k| k.requests as f64),
            series(|k| k.pass_rate),
            series(|k| k.p50),
            series(|k| k.p95),
            series(|k| k.errors as f64),
        ]
    }

    /// The count of each class of [`CLASSES`].
    pub fn outcome_counts(&self) -> [u64; 6] {
        let mut counts = [0; 6];
        for e in self.sent() {
            let class = match (e.outcome, e.status) {
                (Outcome::Errored, _) => Some(4),
                (Outcome::Cancelled, _) => Some(5),
                (_, Some(200..=299)) => Some(0),
                (_, Some(300..=399)) => Some(1),
                (_, Some(400..=499)) => Some(2),
                (_, Some(500..=599)) => Some(3),
                _ => None,
            };
            if let Some(class) = class {
                counts[class] += 1;
            }
        }
        counts
    }

    /// Latency of each sent exchange, by order of completion.
    pub fn latency_points(&self) -> Vec<(f64, f64)> {
        self.sent()
            .filter(|e| e.outcome != Outcome::Cancelled)
            .enumerate()
            .map(|(i, e)| ((i + 1) as f64, e.duration_ms))
            .collect()
    }

    /// Exchanges per latency bucket: (upper bound in ms, count). The last
    /// bucket holds everything above the previous bound.
    pub fn histogram(&self) -> Vec<(f64, u64)> {
        let mut counts = vec![0u64; BUCKETS.len()];
        for e in self.sent().filter(|e| e.outcome != Outcome::Cancelled) {
            let bucket = BUCKETS
                .iter()
                .position(|bound| e.duration_ms <= *bound)
                .unwrap_or(BUCKETS.len() - 1);
            counts[bucket] += 1;
        }
        BUCKETS.iter().copied().zip(counts).collect()
    }

    /// One row per request file, by request id.
    pub fn request_rows(&self) -> Vec<RequestRow> {
        let mut by_request: BTreeMap<&str, Vec<&Exchange>> = BTreeMap::new();
        for e in self.sent() {
            by_request.entry(&e.request).or_default().push(e);
        }
        by_request
            .into_iter()
            .map(|(request, exchanges)| {
                let latencies: Vec<f64> = exchanges.iter().map(|e| e.duration_ms).collect();
                let passed = exchanges
                    .iter()
                    .filter(|e| e.outcome == Outcome::Passed)
                    .count() as u64;
                RequestRow {
                    request: request.to_owned(),
                    count: exchanges.len() as u64,
                    pass_rate: percent(passed, exchanges.len() as u64),
                    p50: percentile(&latencies, 50.0).unwrap_or(0.0),
                    p95: percentile(&latencies, 95.0).unwrap_or(0.0),
                    latencies,
                }
            })
            .collect()
    }

    /// The request table: header and cells.
    pub fn request_table(&self) -> (Vec<String>, Vec<Vec<String>>) {
        let columns = ["Request", "Count", "Pass rate", "p50", "p95", "Latency"]
            .map(str::to_owned)
            .to_vec();
        let rows = self
            .request_rows()
            .into_iter()
            .map(|r| {
                vec![
                    r.request,
                    r.count.to_string(),
                    format_percent(r.pass_rate),
                    format_ms(r.p50),
                    format_ms(r.p95),
                    text_sparkline(&r.latencies, 24),
                ]
            })
            .collect();
        (columns, rows)
    }

    /// The state line: `Running`, `Completed`, `Aborted`, and `Producer
    /// disconnected` when the producer went away before the end.
    pub fn state_text(&self) -> &'static str {
        self.phase.text()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(request: &str, outcome: Outcome, status: Option<u16>, ms: f64) -> Exchange {
        Exchange {
            request: request.into(),
            outcome,
            status,
            duration_ms: ms,
            assertions_passed: u32::from(outcome == Outcome::Passed),
            assertions_failed: u32::from(outcome == Outcome::Failed),
        }
    }

    /// The spec's completed run: 180 × 2xx, 15 × 4xx, 5 transport errors.
    pub fn fixture_180_15_5() -> RunModel {
        let mut run = RunModel::new("api/");
        for i in 0..180 {
            run.exchange(exchange(
                "users/get",
                Outcome::Passed,
                Some(200),
                20.0 + f64::from(i % 50),
            ));
        }
        for _ in 0..15 {
            run.exchange(exchange("users/create", Outcome::Failed, Some(422), 40.0));
        }
        for _ in 0..5 {
            run.exchange(exchange("orders/list", Outcome::Errored, None, 3.0));
        }
        run.phase = Phase::Completed;
        run
    }

    #[test]
    fn the_completed_run_fixture_counts_by_class() {
        let run = fixture_180_15_5();
        assert_eq!(run.outcome_counts(), [180, 0, 15, 0, 5, 0]);
        let tiles = run.tile_texts();
        assert_eq!(tiles[0], "200");
        assert_eq!(tiles[1], "92%"); // 180 of 195 assertions
        assert_eq!(tiles[4], "5");
        assert_eq!(run.state_text(), "Completed");
        let rows = run.request_rows();
        assert_eq!(
            rows.iter()
                .map(|r| (r.request.as_str(), r.count))
                .collect::<Vec<_>>(),
            [("orders/list", 5), ("users/create", 15), ("users/get", 180)]
        );
        assert_eq!(rows[2].pass_rate, 100.0);
        assert_eq!(run.histogram().iter().map(|(_, n)| n).sum::<u64>(), 200);
        assert_eq!(run.latency_points().len(), 200);
    }

    #[test]
    fn while_running_the_requests_tile_shows_the_plan() {
        let mut run = RunModel::new("api/");
        run.started(&RunStarted {
            started_at: "2026-11-19T10:00:00Z".into(),
            environment: Some("dev".into()),
            total: Some(1000),
        });
        run.exchange(exchange("a", Outcome::Passed, Some(200), 10.0));
        assert_eq!(run.tile_texts()[0], "1 / 1000");
        assert_eq!(run.state_text(), "Running");
        assert_eq!(run.tile_history()[0], [1.0]);
        run.finished(&RunFinished {
            state: RunState::Aborted,
            summary: rustman_viz_protocol::RunSummary::default(),
        });
        assert_eq!(run.state_text(), "Aborted");
        assert_eq!(run.tile_texts()[0], "1");
    }

    #[test]
    fn skipped_and_cancelled_exchanges() {
        let mut run = RunModel::new("x");
        run.exchange(exchange("a", Outcome::Skipped, None, 0.0));
        run.exchange(exchange("a", Outcome::Cancelled, None, 0.0));
        assert_eq!(run.outcome_counts(), [0, 0, 0, 0, 0, 1]);
        assert_eq!(run.kpis().requests, 1);
        assert!(run.latency_points().is_empty());
    }
}
