//! Thresholds (load-testing "Thresholds" and "Abort-on-fail thresholds"):
//! `<metric><op><value>`, optionally scoped to one request file and
//! suffixed with `!` to abort the bench as soon as it is breached.
//!
//! | Metric | Value | Example |
//! |--------|-------|---------|
//! | `p50`, `p90`, `p95`, `p99`, `max`, `avg` | a duration (`ms` when bare) | `p95<300ms` |
//! | `error_rate` | a percentage or a fraction | `error_rate<1%` |
//! | `checks` | the assertion pass rate, as a percentage or fraction | `checks>99%` |
//! | `rps` | requests per second | `rps>=50` |
//!
//! Operators: `<`, `<=`, `>`, `>=`. A scope names a request file by its
//! path, its path without `.rman.yaml`, or its file name stem:
//! `file:checkout/login:p95<200ms`.

use std::fmt;
use std::str::FromStr;

use rustman_report::bench::{StatsSummary, ThresholdResult};

use crate::error::BenchError;
use crate::metrics::BenchSummary;

/// The `actual` of a threshold with nothing to measure; it passes.
pub const NOT_MEASURED: &str = "n/a";

/// A latency statistic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stat {
    P50,
    P90,
    P95,
    P99,
    Max,
    Avg,
}

/// What a threshold measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Latency(Stat),
    ErrorRate,
    Rps,
    Checks,
}

impl Metric {
    fn parse(name: &str) -> Result<Self, String> {
        Ok(match name {
            "p50" => Self::Latency(Stat::P50),
            "p90" => Self::Latency(Stat::P90),
            "p95" => Self::Latency(Stat::P95),
            "p99" => Self::Latency(Stat::P99),
            "max" => Self::Latency(Stat::Max),
            "avg" => Self::Latency(Stat::Avg),
            "error_rate" => Self::ErrorRate,
            "rps" => Self::Rps,
            "checks" => Self::Checks,
            other => {
                return Err(format!(
                    "unknown metric '{other}' (expected p50, p90, p95, p99, max, avg, error_rate, rps or checks)"
                ));
            }
        })
    }

    /// Parse `text` as a value of this metric: milliseconds for latency,
    /// a fraction for rates, requests per second for `rps`.
    fn value(self, text: &str) -> Result<f64, String> {
        match self {
            Self::Latency(_) => milliseconds(text),
            Self::ErrorRate | Self::Checks => fraction(text),
            Self::Rps => number(text).ok_or_else(|| format!("'{text}' is not a number")),
        }
    }

    /// `value` as shown in a result: `412ms`, `0.20%`, `48.3`.
    fn show(self, value: f64) -> String {
        match self {
            Self::Latency(_) => format!("{}ms", trim(value)),
            Self::ErrorRate | Self::Checks => format!("{:.2}%", value * 100.0),
            Self::Rps => format!("{value:.1}"),
        }
    }

    /// The measured value of `stats`; `None` when there is nothing to
    /// measure.
    fn measure(self, stats: &StatsSummary, summary: &BenchSummary) -> Option<f64> {
        let l = &stats.latency;
        Some(match self {
            Self::Latency(Stat::P50) => l.p50_ms,
            Self::Latency(Stat::P90) => l.p90_ms,
            Self::Latency(Stat::P95) => l.p95_ms,
            Self::Latency(Stat::P99) => l.p99_ms,
            Self::Latency(Stat::Max) => l.max_ms,
            Self::Latency(Stat::Avg) => l.avg_ms,
            Self::ErrorRate => stats.error_rate,
            Self::Rps => summary.rps_of(stats),
            Self::Checks => return stats.checks.rate(),
        })
    }
}

/// A comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Lt,
    Le,
    Gt,
    Ge,
}

impl Op {
    fn holds(self, measured: f64, limit: f64) -> bool {
        match self {
            Self::Lt => measured < limit,
            Self::Le => measured <= limit,
            Self::Gt => measured > limit,
            Self::Ge => measured >= limit,
        }
    }
}

/// One threshold.
#[derive(Clone, Debug, PartialEq)]
pub struct Threshold {
    /// As written, such as `p95<300ms` or `error_rate<5%!`.
    pub expression: String,
    /// The request file it is scoped to, as written after `file:`.
    pub scope: Option<String>,
    pub metric: Metric,
    pub op: Op,
    /// Milliseconds, a fraction or requests per second, by metric.
    pub value: f64,
    /// Breaching it stops the bench (`!`).
    pub abort: bool,
}

impl FromStr for Threshold {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        parse(text.trim()).map_err(|reason| format!("invalid threshold '{text}': {reason}"))
    }
}

impl fmt::Display for Threshold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.expression)
    }
}

fn parse(expression: &str) -> Result<Threshold, String> {
    const SHAPE: &str = "expected <metric><op><value>, such as p95<300ms";
    let (body, abort) = match expression.strip_suffix('!') {
        Some(body) => (body, true),
        None => (expression, false),
    };
    let at = body.find(['<', '>']).ok_or(SHAPE)?;
    let (left, rest) = body.split_at(at);
    let (op, right) = match (rest.starts_with('<'), rest[1..].strip_prefix('=')) {
        (true, Some(right)) => (Op::Le, right),
        (true, None) => (Op::Lt, &rest[1..]),
        (false, Some(right)) => (Op::Ge, right),
        (false, None) => (Op::Gt, &rest[1..]),
    };
    let (scope, name) = scoped(left.trim())?;
    let metric = Metric::parse(name)?;
    let right = right.trim();
    if right.is_empty() {
        return Err(SHAPE.to_owned());
    }
    Ok(Threshold {
        expression: expression.to_owned(),
        scope,
        metric,
        op,
        value: metric.value(right)?,
        abort,
    })
}

/// `file:<name>:<metric>` or `<metric>`.
fn scoped(left: &str) -> Result<(Option<String>, &str), String> {
    let Some(rest) = left.strip_prefix("file:") else {
        return Ok((None, left));
    };
    match rest.rsplit_once(':') {
        Some((name, metric)) if !name.is_empty() => Ok((Some(name.to_owned()), metric)),
        _ => Err("a scope is file:<name>:<metric>, such as file:login:p95<200ms".to_owned()),
    }
}

/// A duration in milliseconds: `300ms`, `1.5s`, `2m`, `500us`, or bare
/// milliseconds. The number may use exponent notation (`1e3`).
fn milliseconds(text: &str) -> Result<f64, String> {
    let invalid = || format!("'{text}' is not a duration, such as 300ms or 1.5s");
    // Longer units first: `ms` and `us` also end with `s`.
    const UNITS: [(&str, f64); 5] = [
        ("ms", 1.0),
        ("us", 0.001),
        ("µs", 0.001),
        ("s", 1000.0),
        ("m", 60_000.0),
    ];
    let (number_part, scale) = UNITS
        .iter()
        .find_map(|(unit, scale)| text.strip_suffix(unit).map(|n| (n, *scale)))
        .unwrap_or((text, 1.0));
    let value = number(number_part).ok_or_else(invalid)?;
    Ok(value * scale)
}

/// A rate from 0 to 1: `1%`, `0.5 %` or a bare fraction.
fn fraction(text: &str) -> Result<f64, String> {
    let invalid = || format!("'{text}' is not a rate, such as 1% or 0.01");
    let rate = match text.strip_suffix('%') {
        Some(percent) => number(percent.trim()).map(|p| p / 100.0),
        None => number(text),
    };
    rate.filter(|f| *f <= 1.0).ok_or_else(invalid)
}

/// A finite, non-negative number.
fn number(text: &str) -> Option<f64> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
}

/// `412`, `12.5`, `0.25`: at most two decimals, no trailing zeros.
fn trim(value: f64) -> String {
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

impl Threshold {
    /// The statistics this threshold looks at: its file's, or overall.
    fn stats<'s>(&self, summary: &'s BenchSummary) -> Option<&'s StatsSummary> {
        match &self.scope {
            None => Some(&summary.overall),
            Some(name) => summary
                .files
                .iter()
                .find(|f| names(&f.file, name))
                .map(|f| &f.stats),
        }
    }

    /// Whether `summary` keeps to this threshold, with the measured value.
    /// A metric with nothing to measure (`checks` without assertions)
    /// passes as `n/a`.
    pub fn evaluate(&self, summary: &BenchSummary) -> ThresholdResult {
        let measured = self
            .stats(summary)
            .and_then(|stats| self.metric.measure(stats, summary));
        let (passed, actual) = match measured {
            Some(value) => (self.op.holds(value, self.value), self.metric.show(value)),
            None => (true, NOT_MEASURED.to_owned()),
        };
        ThresholdResult {
            expression: self.expression.clone(),
            passed,
            actual,
            abort: self.abort,
        }
    }
}

/// Whether request file `file` (a workspace path with `/`) is the one
/// `name` designates: the path itself, the path without `.rman.yaml`, or
/// its file name stem.
pub fn names(file: &str, name: &str) -> bool {
    let id = file.strip_suffix(".rman.yaml").unwrap_or(file);
    let stem = id.rsplit('/').next().unwrap_or(id);
    file == name || id == name || stem == name
}

/// Refuse a threshold scoped to a file the bench does not run.
pub fn check_scopes(thresholds: &[Threshold], files: &[String]) -> Result<(), BenchError> {
    for threshold in thresholds {
        if let Some(scope) = &threshold.scope
            && !files.iter().any(|f| names(f, scope))
        {
            return Err(BenchError::Usage(format!(
                "bench: threshold '{threshold}' names file '{scope}', which the bench does not run"
            )));
        }
    }
    Ok(())
}

/// Every threshold evaluated against `summary`, in order.
pub fn evaluate_all(thresholds: &[Threshold], summary: &BenchSummary) -> Vec<ThresholdResult> {
    thresholds.iter().map(|t| t.evaluate(summary)).collect()
}

#[cfg(test)]
mod tests {
    use rustman_report::bench::{Checks, FileSummary, LatencyKind, LatencySummary};

    use super::*;

    fn t(text: &str) -> Threshold {
        text.parse().unwrap()
    }

    /// An expression, then its scope, metric, operator, value and `!`.
    type Case = (&'static str, Option<&'static str>, Metric, Op, f64, bool);

    #[test]
    fn thresholds_parse() {
        let cases: [Case; 14] = [
            (
                "p95<1e3",
                None,
                Metric::Latency(Stat::P95),
                Op::Lt,
                1000.0,
                false,
            ),
            (
                "p95<2.5e-1s",
                None,
                Metric::Latency(Stat::P95),
                Op::Lt,
                250.0,
                false,
            ),
            (
                "p95<300ms",
                None,
                Metric::Latency(Stat::P95),
                Op::Lt,
                300.0,
                false,
            ),
            (
                "p99 <= 1.5s",
                None,
                Metric::Latency(Stat::P99),
                Op::Le,
                1500.0,
                false,
            ),
            (
                "p50<250",
                None,
                Metric::Latency(Stat::P50),
                Op::Lt,
                250.0,
                false,
            ),
            (
                "p90<500us",
                None,
                Metric::Latency(Stat::P90),
                Op::Lt,
                0.5,
                false,
            ),
            (
                "max<2m",
                None,
                Metric::Latency(Stat::Max),
                Op::Lt,
                120_000.0,
                false,
            ),
            (
                "avg>=1ms",
                None,
                Metric::Latency(Stat::Avg),
                Op::Ge,
                1.0,
                false,
            ),
            (
                "error_rate<1%",
                None,
                Metric::ErrorRate,
                Op::Lt,
                0.01,
                false,
            ),
            (
                "error_rate<5%!",
                None,
                Metric::ErrorRate,
                Op::Lt,
                0.05,
                true,
            ),
            (
                "error_rate<=0.02",
                None,
                Metric::ErrorRate,
                Op::Le,
                0.02,
                false,
            ),
            ("checks>99.5%", None, Metric::Checks, Op::Gt, 0.995, false),
            ("rps>=50", None, Metric::Rps, Op::Ge, 50.0, false),
            (
                "file:checkout/login:p95<200ms!",
                Some("checkout/login"),
                Metric::Latency(Stat::P95),
                Op::Lt,
                200.0,
                true,
            ),
        ];
        for (text, scope, metric, op, value, abort) in cases {
            let parsed = t(text);
            assert_eq!(parsed.expression, text);
            assert_eq!(parsed.scope.as_deref(), scope, "{text}");
            assert_eq!((parsed.metric, parsed.op), (metric, op), "{text}");
            assert!(
                (parsed.value - value).abs() < 1e-9,
                "{text}: {}",
                parsed.value
            );
            assert_eq!(parsed.abort, abort, "{text}");
        }
    }

    #[test]
    fn invalid_thresholds_say_why() {
        let cases = [
            ("p95", "expected <metric><op><value>"),
            ("p95<", "expected <metric><op><value>"),
            ("p96<300ms", "unknown metric 'p96'"),
            ("p95<fast", "'fast' is not a duration"),
            ("p95<3h", "'3h' is not a duration"),
            ("p95<-3ms", "'-3ms' is not a duration"),
            ("error_rate<lots", "'lots' is not a rate"),
            ("error_rate<5", "'5' is not a rate"),
            ("error_rate<150%", "'150%' is not a rate"),
            ("checks>100.5%", "'100.5%' is not a rate"),
            ("rps>many", "'many' is not a number"),
            ("file:p95<1ms", "a scope is file:<name>:<metric>"),
        ];
        for (text, reason) in cases {
            let err = text.parse::<Threshold>().unwrap_err();
            assert!(
                err.starts_with(&format!("invalid threshold '{text}': ")),
                "{err}"
            );
            assert!(err.contains(reason), "{text}: {err}");
        }
    }

    fn stats(p95: f64, requests: u64, errors: u64, checks: Checks) -> StatsSummary {
        StatsSummary {
            requests,
            errors: [("5xx".to_owned(), errors)].into(),
            error_rate: errors as f64 / requests as f64,
            latency: LatencySummary {
                p95_ms: p95,
                ..LatencySummary::default()
            },
            checks,
            ..StatsSummary::default()
        }
    }

    fn summary() -> BenchSummary {
        let login = stats(120.0, 500, 0, Checks::default());
        let pay = stats(412.0, 500, 2, Checks::default());
        BenchSummary {
            latency_kind: LatencyKind::Service,
            elapsed_ms: 10_000.0,
            iterations: 500,
            iteration_duration: LatencySummary::default(),
            interrupted: 0,
            scheduled: 0,
            dropped: 0,
            rps: 100.0,
            overall: stats(
                412.0,
                1000,
                2,
                Checks {
                    passed: 999,
                    failed: 1,
                },
            ),
            files: vec![
                FileSummary {
                    file: "checkout/login.rman.yaml".into(),
                    stats: login,
                },
                FileSummary {
                    file: "checkout/pay.rman.yaml".into(),
                    stats: pay,
                },
            ],
        }
    }

    #[test]
    fn thresholds_evaluate_against_the_summary() {
        let summary = summary();
        let cases = [
            ("p95<300ms", false, "✗ p95<300ms (412ms)"),
            ("error_rate<1%", true, "✓ error_rate<1% (0.20%)"),
            ("rps>=100", true, "✓ rps>=100 (100.0)"),
            ("checks>99.95%", false, "✗ checks>99.95% (99.90%)"),
            (
                "file:login:p95<200ms",
                true,
                "✓ file:login:p95<200ms (120ms)",
            ),
            (
                "file:checkout/pay.rman.yaml:rps<10",
                false,
                "✗ file:checkout/pay.rman.yaml:rps<10 (50.0)",
            ),
            (
                "file:login:checks>99%",
                true,
                "✓ file:login:checks>99% (n/a)",
            ),
        ];
        for (text, passed, line) in cases {
            let result = t(text).evaluate(&summary);
            assert_eq!(result.passed, passed, "{text}");
            assert_eq!(result.line(), line);
        }
    }

    #[test]
    fn scopes_must_name_a_file_of_the_bench() {
        let files = vec!["checkout/login.rman.yaml".to_owned()];
        assert!(check_scopes(&[t("file:login:p95<1ms")], &files).is_ok());
        assert!(check_scopes(&[t("file:checkout/login:p95<1ms")], &files).is_ok());
        let err = check_scopes(&[t("file:pay:p95<1ms")], &files).unwrap_err();
        assert_eq!(
            err.to_string(),
            "bench: threshold 'file:pay:p95<1ms' names file 'pay', which the bench does not run"
        );
    }
}
