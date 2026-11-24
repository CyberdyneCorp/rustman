//! The bench dashboard's data (`rustman bench --viz`, issue #123): KPI
//! tiles with their per-second history, throughput, latency percentiles
//! and active VUs over time, the totals and the thresholds. Fed by
//! `bench_tick` and `bench_finished`, or by a saved bench report.
//!
//! Ticks hold counts and timings only. The producer drops ticks when the
//! companion falls behind, so seconds may be missing: plots are keyed by
//! second, and totals come from `bench_finished` once it arrives.

use rustman_viz_protocol::{BenchFinished, BenchThreshold, BenchTick, RunState};

use super::Phase;
use super::stats::{format_ms, format_percent, percent};

/// How many seconds the KPI sparklines keep.
const HISTORY: usize = 120;

/// The bench dashboard.
#[derive(Debug, Clone, Default)]
pub struct BenchModel {
    pub title: String,
    /// The `--target` origin, when known (reports).
    pub target: Option<String>,
    pub environment: Option<String>,
    /// The load profile in words, such as `constant-vus · 5 VUs · 3 s`.
    pub profile: Option<String>,
    /// The seconds received, by second.
    pub seconds: Vec<BenchTick>,
    /// The totals, once the bench is over.
    pub finished: Option<BenchFinished>,
    pub phase: Phase,
}

impl BenchModel {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }

    /// One closed second. A repeated second replaces the earlier one.
    pub fn tick(&mut self, tick: &BenchTick) {
        match self.seconds.iter().position(|s| s.second >= tick.second) {
            Some(i) if self.seconds[i].second == tick.second => self.seconds[i] = tick.clone(),
            Some(i) => self.seconds.insert(i, tick.clone()),
            None => self.seconds.push(tick.clone()),
        }
    }

    pub fn finished(&mut self, finished: &BenchFinished) {
        self.phase = match finished.state {
            RunState::Aborted => Phase::Aborted,
            _ if finished.aborted.is_some() => Phase::Aborted,
            _ => Phase::Completed,
        };
        self.finished = Some(finished.clone());
    }

    fn last(&self) -> Option<&BenchTick> {
        self.seconds.last()
    }

    /// Requests and errors so far: the totals once finished, else the sum
    /// of the seconds received.
    pub fn counts(&self) -> (u64, u64) {
        match &self.finished {
            Some(f) => (f.requests, f.errors),
            None => self
                .seconds
                .iter()
                .fold((0, 0), |(r, e), s| (r + s.requests, e + s.errors)),
        }
    }

    /// The tile texts: requests, requests per second, p95 latency, error
    /// rate and active VUs. While running, the rate and the latency are
    /// the last second's; once finished, the whole bench's.
    pub fn tile_texts(&self) -> [String; 5] {
        let (requests, errors) = self.counts();
        let (rps, p95) = match (&self.finished, self.last()) {
            (Some(f), _) => (Some(f.rps), Some(f.p95_ms)),
            (None, Some(s)) => (Some(s.rps), Some(s.p95_ms)),
            (None, None) => (None, None),
        };
        let dash = || "–".to_owned();
        [
            requests.to_string(),
            rps.map_or_else(dash, |r| format!("{r:.1}")),
            p95.map_or_else(dash, format_ms),
            if requests == 0 {
                dash()
            } else {
                format_percent(percent(errors, requests))
            },
            self.last().map_or_else(dash, |s| s.active_vus.to_string()),
        ]
    }

    /// The sparkline values of each tile, the last [`HISTORY`] seconds.
    pub fn tile_history(&self) -> [Vec<f32>; 5] {
        let start = self.seconds.len().saturating_sub(HISTORY);
        let shown = &self.seconds[start..];
        let series = |f: fn(&BenchTick) -> f64| shown.iter().map(|s| f(s) as f32).collect();
        [
            series(|s| s.requests as f64),
            series(|s| s.rps),
            series(|s| s.p95_ms),
            series(|s| percent(s.errors, s.requests)),
            series(|s| f64::from(s.active_vus)),
        ]
    }

    fn points(&self, f: impl Fn(&BenchTick) -> f64) -> Vec<(f64, f64)> {
        self.seconds
            .iter()
            .map(|s| (f64::from(s.second), f(s)))
            .collect()
    }

    /// Requests per second and errors per second, by second.
    pub fn throughput(&self) -> [(&'static str, Vec<(f64, f64)>); 2] {
        [
            ("requests/s", self.points(|s| s.rps)),
            ("errors", self.points(|s| s.errors as f64)),
        ]
    }

    /// p50, p95 and p99 latency, by second.
    pub fn latency(&self) -> [(&'static str, Vec<(f64, f64)>); 3] {
        [
            ("p50", self.points(|s| s.p50_ms)),
            ("p95", self.points(|s| s.p95_ms)),
            ("p99", self.points(|s| s.p99_ms)),
        ]
    }

    /// Active VUs, by second.
    pub fn vus(&self) -> Vec<(f64, f64)> {
        self.points(|s| f64::from(s.active_vus))
    }

    /// The subtitle: target, environment, profile, then elapsed and
    /// remaining time.
    pub fn subtitle(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        parts.extend(self.target.clone());
        parts.extend(
            self.environment
                .as_ref()
                .map(|e| format!("environment {e}")),
        );
        parts.extend(self.profile.clone());
        let elapsed = match (&self.finished, self.last()) {
            (Some(f), _) => Some(f.duration_ms),
            (None, Some(s)) => Some(s.elapsed_ms),
            (None, None) => None,
        };
        if let Some(ms) = elapsed {
            parts.push(format!("{} elapsed", seconds(ms)));
        }
        if self.finished.is_none()
            && let Some(left) = self.last().and_then(|s| s.remaining_ms)
        {
            parts.push(format!("{} left", seconds(left)));
        }
        parts.join(" · ")
    }

    /// `Running`, `Completed`, `Aborted (threshold)`, ...
    pub fn state_text(&self) -> String {
        let reason = self.finished.as_ref().and_then(|f| f.aborted.as_deref());
        match (self.phase, reason) {
            (Phase::Aborted, Some(reason)) => format!("Aborted ({reason})"),
            (phase, _) => phase.text().to_owned(),
        }
    }

    /// The totals table: metric and value. Before `bench_finished` it
    /// shows what the seconds received add up to.
    pub fn summary_table(&self) -> (Vec<String>, Vec<Vec<String>>) {
        let columns = ["Metric", "Value"].map(str::to_owned).to_vec();
        let (requests, errors) = self.counts();
        let row = |k: &str, v: String| vec![k.to_owned(), v];
        let mut rows = Vec::new();
        match &self.finished {
            Some(f) => {
                rows.push(row("Duration", seconds(f.duration_ms)));
                rows.push(row("Iterations", f.iterations.to_string()));
                rows.push(row("Requests", requests.to_string()));
                rows.push(row("Errors", errors.to_string()));
                rows.push(row("Requests/s", format!("{:.1}", f.rps)));
                rows.push(row("p50 latency", format_ms(f.p50_ms)));
                rows.push(row("p95 latency", format_ms(f.p95_ms)));
                rows.push(row("p99 latency", format_ms(f.p99_ms)));
            }
            None => {
                let elapsed = self.last().map_or(0, |s| s.elapsed_ms);
                rows.push(row("Duration", seconds(elapsed)));
                rows.push(row("Requests", requests.to_string()));
                rows.push(row("Errors", errors.to_string()));
            }
        }
        (columns, rows)
    }

    /// The thresholds table, known when the bench ends.
    pub fn threshold_table(&self) -> (Vec<String>, Vec<Vec<String>>) {
        let columns = ["Threshold", "Result", "Measured"]
            .map(str::to_owned)
            .to_vec();
        let rows = self
            .thresholds()
            .iter()
            .map(|t| {
                vec![
                    t.expression.clone(),
                    if t.passed { "✓ passed" } else { "✗ failed" }.to_owned(),
                    t.actual.clone(),
                ]
            })
            .collect();
        (columns, rows)
    }

    fn thresholds(&self) -> &[BenchThreshold] {
        self.finished.as_ref().map_or(&[], |f| &f.thresholds)
    }

    /// `Thresholds: 1 of 2 failed`, `Thresholds: all 2 passed`, ...
    pub fn thresholds_heading(&self) -> String {
        let all = self.thresholds();
        let failed = all.iter().filter(|t| !t.passed).count();
        match (self.finished.is_some(), all.len(), failed) {
            (false, _, _) => "Thresholds (checked when the bench ends)".to_owned(),
            (true, 0, _) => "Thresholds: none".to_owned(),
            (true, n, 0) => format!("Thresholds: all {n} passed"),
            (true, n, f) => format!("Thresholds: {f} of {n} failed"),
        }
    }
}

/// Milliseconds as whole seconds, or minutes and seconds: `30 s`, `2m 05s`.
fn seconds(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{s} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(second: u32, requests: u64, errors: u64, p95: f64) -> BenchTick {
        BenchTick {
            second,
            elapsed_ms: u64::from(second) * 1000,
            remaining_ms: Some(u64::from(30 - second) * 1000),
            active_vus: 10,
            requests,
            errors,
            rps: requests as f64,
            p50_ms: p95 / 3.0,
            p95_ms: p95,
            p99_ms: p95 * 2.0,
        }
    }

    fn finished(aborted: Option<&str>) -> BenchFinished {
        BenchFinished {
            state: if aborted.is_some() {
                RunState::Aborted
            } else {
                RunState::Completed
            },
            duration_ms: 30_000,
            iterations: 4100,
            requests: 12_300,
            errors: 41,
            rps: 410.0,
            p50_ms: 18.0,
            p95_ms: 64.3,
            p99_ms: 130.1,
            thresholds: vec![
                BenchThreshold {
                    expression: "p95<300ms".into(),
                    passed: true,
                    actual: "64.3ms".into(),
                },
                BenchThreshold {
                    expression: "error_rate<0.1%".into(),
                    passed: false,
                    actual: "0.33%".into(),
                },
            ],
            aborted: aborted.map(str::to_owned),
        }
    }

    #[test]
    fn a_live_bench_shows_the_last_second() {
        let mut bench = BenchModel::new("bench");
        assert_eq!(bench.tile_texts(), ["0", "–", "–", "–", "–"]);
        bench.tick(&tick(1, 400, 0, 60.0));
        bench.tick(&tick(2, 412, 2, 61.0));
        assert_eq!(bench.tile_texts(), ["812", "412.0", "61 ms", "0.2%", "10"]);
        assert_eq!(bench.state_text(), "Running");
        assert_eq!(bench.subtitle(), "2 s elapsed · 28 s left");
        assert_eq!(bench.tile_history()[0], [400.0, 412.0]);
        assert_eq!(bench.vus(), [(1.0, 10.0), (2.0, 10.0)]);
        let [(_, p50), (_, p95), (_, p99)] = bench.latency();
        assert_eq!(p95, [(1.0, 60.0), (2.0, 61.0)]);
        assert_eq!(p50.len(), 2);
        assert_eq!(p99[1], (2.0, 122.0));
        let (_, rows) = bench.summary_table();
        assert_eq!(rows[1], ["Requests", "812"]);
        assert!(bench.threshold_table().1.is_empty());
        assert_eq!(
            bench.thresholds_heading(),
            "Thresholds (checked when the bench ends)"
        );
    }

    #[test]
    fn dropped_and_repeated_seconds_keep_the_series_in_order() {
        let mut bench = BenchModel::new("bench");
        bench.tick(&tick(1, 10, 0, 5.0));
        bench.tick(&tick(4, 40, 0, 5.0));
        bench.tick(&tick(3, 30, 0, 5.0));
        bench.tick(&tick(4, 41, 0, 5.0));
        let seconds: Vec<u32> = bench.seconds.iter().map(|s| s.second).collect();
        assert_eq!(seconds, [1, 3, 4]);
        assert_eq!(bench.counts(), (81, 0));
    }

    #[test]
    fn the_end_shows_totals_and_thresholds() {
        let mut bench = BenchModel::new("bench");
        bench.tick(&tick(1, 400, 0, 60.0));
        bench.finished(&finished(None));
        assert_eq!(bench.state_text(), "Completed");
        assert_eq!(
            bench.tile_texts(),
            ["12300", "410.0", "64 ms", "0.3%", "10"]
        );
        assert_eq!(bench.subtitle(), "30 s elapsed", "no time left once over");
        let (columns, rows) = bench.threshold_table();
        assert_eq!(columns, ["Threshold", "Result", "Measured"]);
        assert_eq!(rows[1], ["error_rate<0.1%", "✗ failed", "0.33%"]);
        assert_eq!(bench.thresholds_heading(), "Thresholds: 1 of 2 failed");
        let (_, rows) = bench.summary_table();
        assert_eq!(rows[1], ["Iterations", "4100"]);
        assert_eq!(rows[7], ["p99 latency", "130 ms"]);
    }

    #[test]
    fn an_aborted_bench_names_why() {
        let mut bench = BenchModel::new("bench");
        bench.finished(&finished(Some("threshold")));
        assert_eq!(bench.phase, Phase::Aborted);
        assert_eq!(bench.state_text(), "Aborted (threshold)");
        assert_eq!(seconds(125_000), "2m 05s");
    }
}
