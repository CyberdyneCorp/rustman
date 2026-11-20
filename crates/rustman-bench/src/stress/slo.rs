//! SLOs (stress-testing "SLO definitions"): `--slo "<metric><op><value>"`
//! or `rustman.toml [stress.slo]`, with the metrics of the bench thresholds
//! (`p50|p90|p95|p99|max|avg`, `error_rate`, `rps`, `checks`), optionally
//! scoped to one request file as `file:<name> <expr>` (or the threshold
//! form `file:<name>:<expr>`). They are evaluated over each window with
//! [`Threshold::evaluate`]. Without any SLO, breakpoint and spike use
//! `p95<1s` and `error_rate<5%`.

use std::fmt;
use std::str::FromStr;

use crate::error::BenchError;
use crate::stress::ModeKind;
use crate::stress::window::WindowStats;
use crate::threshold::{Metric, Threshold, names};

/// The SLOs of breakpoint and spike runs that set none.
pub const DEFAULT_SLOS: [&str; 2] = ["p95<1s", "error_rate<5%"];

/// One SLO.
#[derive(Clone, Debug, PartialEq)]
pub struct Slo {
    /// As written, such as `file:checkout p99<800ms`.
    pub expression: String,
    pub threshold: Threshold,
}

impl FromStr for Slo {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let invalid = |reason: String| format!("invalid SLO '{text}': {reason}");
        if text.ends_with('!') {
            return Err(invalid(
                "an SLO has no abort marker '!' (a broken SLO ends the step, not the run)".into(),
            ));
        }
        let threshold: Threshold = normalized(text).parse().map_err(|e: String| {
            invalid(
                e.split_once("': ")
                    .map_or(e.clone(), |(_, reason)| reason.to_owned()),
            )
        })?;
        Ok(Self {
            expression: text.to_owned(),
            threshold,
        })
    }
}

impl fmt::Display for Slo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.expression)
    }
}

/// `file:<name> <expr>` as the threshold form `file:<name>:<expr>`.
fn normalized(text: &str) -> String {
    match text
        .strip_prefix("file:")
        .and_then(|rest| rest.split_once(char::is_whitespace))
    {
        Some((name, expr)) => format!("file:{name}:{}", expr.trim()),
        None => text.to_owned(),
    }
}

/// An SLO a window failed.
#[derive(Clone, Debug, PartialEq)]
pub struct SloFailure {
    /// The SLO as written.
    pub expression: String,
    pub metric: Metric,
    /// The measured value, such as `412ms`.
    pub actual: String,
    /// The file the SLO is scoped to, as written.
    pub file: Option<String>,
}

/// The SLOs of a run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Slos {
    list: Vec<Slo>,
}

impl Slos {
    /// `config` (`[stress.slo]` expressions) then `flags`; when both are
    /// empty, the defaults of `mode` (breakpoint and spike only). A scope
    /// must name a file of `files`.
    pub fn new(
        config: &[String],
        flags: &[Slo],
        mode: ModeKind,
        files: &[String],
    ) -> Result<Self, BenchError> {
        let mut list = Vec::with_capacity(config.len() + flags.len());
        for expression in config {
            let slo = expression
                .parse()
                .map_err(|e| BenchError::Usage(format!("rustman.toml [stress.slo]: {e}")))?;
            list.push(slo);
        }
        list.extend(flags.iter().cloned());
        if list.is_empty() && matches!(mode, ModeKind::Breakpoint | ModeKind::Spike) {
            list = DEFAULT_SLOS
                .iter()
                .map(|s| s.parse().expect("the default SLOs parse"))
                .collect();
        }
        for slo in &list {
            if let Some(scope) = &slo.threshold.scope
                && !files.iter().any(|f| names(f, scope))
            {
                return Err(BenchError::Usage(format!(
                    "stress: SLO '{slo}' names file '{scope}', which the run does not send"
                )));
            }
        }
        Ok(Self { list })
    }

    pub fn list(&self) -> &[Slo] {
        &self.list
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The SLOs `window` failed, in order.
    pub fn evaluate(&self, window: &WindowStats) -> Vec<SloFailure> {
        let summary = window.summary();
        self.list
            .iter()
            .filter_map(|slo| {
                let result = slo.threshold.evaluate(&summary);
                (!result.passed).then(|| SloFailure {
                    expression: slo.expression.clone(),
                    metric: slo.threshold.metric,
                    actual: result.actual,
                    file: slo.threshold.scope.clone(),
                })
            })
            .collect()
    }

    /// The SLOs that apply to request file `file` on its own: those scoped
    /// to it, and the unscoped latency and error-rate ones.
    pub fn for_file<'s>(&'s self, file: &'s str) -> impl Iterator<Item = &'s Slo> + 's {
        self.list
            .iter()
            .filter(move |slo| match &slo.threshold.scope {
                Some(scope) => names(file, scope),
                None => matches!(slo.threshold.metric, Metric::Latency(_) | Metric::ErrorRate),
            })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use rustman_report::bench::{FileSummary, LatencySummary, StatsSummary};

    use super::*;

    fn stats(p99_ms: f64, p95_ms: f64, requests: u64, errors: u64) -> StatsSummary {
        let mut map = BTreeMap::new();
        if errors > 0 {
            map.insert("5xx".to_owned(), errors);
        }
        StatsSummary {
            requests,
            errors: map,
            error_rate: errors as f64 / requests.max(1) as f64,
            latency: LatencySummary {
                p95_ms,
                p99_ms,
                ..LatencySummary::default()
            },
            ..StatsSummary::default()
        }
    }

    fn window(files: Vec<(&str, StatsSummary)>, overall: StatsSummary) -> WindowStats {
        WindowStats {
            length: Duration::from_secs(5),
            scheduled: overall.requests,
            requests_per_iteration: files.len(),
            overall,
            files: files
                .into_iter()
                .map(|(file, stats)| FileSummary {
                    file: file.to_owned(),
                    stats,
                })
                .collect(),
            throttled: 0,
            throttled_5xx: 0,
            fuzzed: 0,
            in_flight: 0,
        }
    }

    const FILES: [&str; 2] = ["api/checkout.rman.yaml", "api/list.rman.yaml"];

    fn files() -> Vec<String> {
        FILES.iter().map(|f| (*f).to_owned()).collect()
    }

    fn slos(flags: &[&str], mode: ModeKind) -> Slos {
        let flags: Vec<Slo> = flags.iter().map(|s| s.parse().unwrap()).collect();
        Slos::new(&[], &flags, mode, &files()).unwrap()
    }

    #[test]
    fn a_scoped_slo_fails_naming_its_file() {
        let slos = slos(&["file:checkout p99<800ms"], ModeKind::Breakpoint);
        let w = window(
            vec![
                (FILES[0], stats(950.0, 900.0, 10, 0)),
                (FILES[1], stats(20.0, 15.0, 10, 0)),
            ],
            stats(940.0, 600.0, 20, 0),
        );
        let failures = slos.evaluate(&w);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].expression, "file:checkout p99<800ms");
        assert_eq!(failures[0].file.as_deref(), Some("checkout"));
        assert_eq!(failures[0].actual, "950ms");
        let calm = window(
            vec![
                (FILES[0], stats(700.0, 600.0, 10, 0)),
                (FILES[1], stats(950.0, 900.0, 10, 0)),
            ],
            stats(940.0, 600.0, 20, 0),
        );
        assert!(slos.evaluate(&calm).is_empty());
    }

    /// SLOs, overall p95, requests, errors, and the failed expressions.
    type Row = (
        &'static [&'static str],
        f64,
        u64,
        u64,
        &'static [&'static str],
    );

    #[test]
    fn windows_are_judged_against_every_slo() {
        // 100 requests in a 5-second window: 20 req/s.
        let rows: [Row; 7] = [
            (&[], 200.0, 100, 0, &[]),
            (&[], 1200.0, 100, 0, &["p95<1s"]),
            (&[], 200.0, 100, 6, &["error_rate<5%"]),
            (&[], 1500.0, 100, 50, &["p95<1s", "error_rate<5%"]),
            (&["p95<300ms"], 400.0, 100, 50, &["p95<300ms"]),
            (&["rps>=30"], 10.0, 100, 0, &["rps>=30"]),
            (&["rps>=20"], 10.0, 100, 0, &[]),
        ];
        for (flags, p95, requests, errors, expected) in rows {
            let slos = slos(flags, ModeKind::Spike);
            let w = window(vec![], stats(p95, p95, requests, errors));
            let failed: Vec<String> = slos
                .evaluate(&w)
                .into_iter()
                .map(|f| f.expression)
                .collect();
            assert_eq!(failed, expected, "{flags:?} p95={p95} errors={errors}");
        }
    }

    #[test]
    fn defaults_apply_to_breakpoint_and_spike_only() {
        let expressions = |mode| -> Vec<String> {
            slos(&[], mode)
                .list()
                .iter()
                .map(|s| s.expression.clone())
                .collect()
        };
        assert_eq!(expressions(ModeKind::Breakpoint), DEFAULT_SLOS);
        assert_eq!(expressions(ModeKind::Spike), DEFAULT_SLOS);
        assert!(expressions(ModeKind::Soak).is_empty());
        assert!(expressions(ModeKind::Fuzz).is_empty());
        let config = ["p99<2s".to_owned()];
        let mixed = Slos::new(
            &config,
            &["error_rate<1%".parse().unwrap()],
            ModeKind::Breakpoint,
            &files(),
        )
        .unwrap();
        let written: Vec<&str> = mixed.list().iter().map(|s| s.expression.as_str()).collect();
        assert_eq!(written, ["p99<2s", "error_rate<1%"]);
    }

    #[test]
    fn bad_slos_are_usage_errors() {
        assert!("p95<300ms!".parse::<Slo>().unwrap_err().contains("abort"));
        assert_eq!(
            "lag<3s".parse::<Slo>().unwrap_err(),
            "invalid SLO 'lag<3s': unknown metric 'lag' (expected p50, p90, p95, p99, max, avg, error_rate, rps or checks)"
        );
        let other = ["file:search p95<1s".parse().unwrap()];
        let err = Slos::new(&[], &other, ModeKind::Breakpoint, &files()).unwrap_err();
        assert!(!err.is_refusal());
        assert!(err.to_string().starts_with("stress: SLO"), "{err}");
        let err = Slos::new(&["p95=3".into()], &[], ModeKind::Soak, &files()).unwrap_err();
        assert!(err.to_string().contains("[stress.slo]"), "{err}");
    }

    #[test]
    fn a_file_is_judged_by_its_own_and_the_shared_latency_slos() {
        let slos = slos(
            &["p95<1s", "rps>=10", "file:checkout p99<800ms"],
            ModeKind::Breakpoint,
        );
        let checkout: Vec<&str> = slos
            .for_file(FILES[0])
            .map(|s| s.expression.as_str())
            .collect();
        assert_eq!(checkout, ["p95<1s", "file:checkout p99<800ms"]);
        let list: Vec<&str> = slos
            .for_file(FILES[1])
            .map(|s| s.expression.as_str())
            .collect();
        assert_eq!(list, ["p95<1s"]);
    }
}
