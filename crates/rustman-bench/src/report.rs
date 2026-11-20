//! The bench report (`rustman.report/v1`, `kind: bench`) of a finished
//! bench. Its texts come from the metrics, whose error samples were
//! redacted when they were recorded; the profile, target and file paths
//! hold no secret.

use std::time::Duration;

use rustman_report::bench::{BenchProfile, BenchReport, BenchTotals, KIND_BENCH, StageProfile};

use crate::bench::BenchOutcome;
use crate::profile::{Limit, Profile, ThinkTime, format_duration};

/// What the report says about the run besides its measures.
pub struct ReportContext<'a> {
    /// The `--target` origin.
    pub target: &'a str,
    pub environment: Option<&'a str>,
    pub profile: &'a Profile,
}

/// The report of `outcome`.
pub fn bench_report(outcome: &BenchOutcome, context: &ReportContext<'_>) -> BenchReport {
    let s = &outcome.summary;
    BenchReport {
        schema: rustman_report::SCHEMA.to_owned(),
        kind: KIND_BENCH.to_owned(),
        rustman_version: rustman_core::VERSION.to_owned(),
        started_at: outcome.started_at.clone(),
        finished_at: outcome.finished_at.clone(),
        target: context.target.to_owned(),
        environment: context.environment.map(str::to_owned),
        profile: profile(context.profile),
        aborted: outcome.aborted,
        totals: BenchTotals {
            latency_kind: s.latency_kind,
            duration_ms: s.elapsed_ms,
            iterations: s.iterations,
            iteration_duration: s.iteration_duration,
            interrupted: s.interrupted,
            scheduled: s.scheduled,
            dropped: s.dropped,
            rps: s.rps,
            generator_lag_p99_ms: outcome.generator_lag.as_secs_f64() * 1000.0,
        },
        overall: s.overall.clone(),
        files: s.files.clone(),
        series: outcome.series.clone(),
        thresholds: outcome.thresholds.clone(),
        error_samples: outcome.error_samples.clone(),
        warnings: outcome.warnings.clone(),
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn think(think: ThinkTime) -> Option<String> {
    Some(match think {
        ThinkTime::None => "0".to_owned(),
        ThinkTime::Fixed(d) => format_duration(d),
        ThinkTime::Range(min, max) => format!("{}..{}", format_duration(min), format_duration(max)),
    })
}

/// The profile as the report shows it.
pub fn profile(profile: &Profile) -> BenchProfile {
    let executor = profile.executor().name().to_owned();
    match profile {
        Profile::ConstantVus(p) => {
            let (duration_ms, iterations) = match p.limit {
                Limit::Duration(d) => (Some(millis(d)), None),
                Limit::Iterations(n) => (None, Some(n)),
            };
            BenchProfile {
                executor,
                vus: Some(p.vus),
                duration_ms,
                iterations,
                think_time: think(p.think),
                ..BenchProfile::default()
            }
        }
        Profile::RampingVus(p) => BenchProfile {
            executor,
            duration_ms: profile.duration().map(millis),
            think_time: think(p.think),
            stages: p
                .stages
                .iter()
                .map(|s| StageProfile {
                    duration_ms: millis(s.duration),
                    target: s.target,
                })
                .collect(),
            ..BenchProfile::default()
        },
        Profile::ConstantArrivalRate(p) => BenchProfile {
            executor,
            duration_ms: Some(millis(p.duration)),
            rate: Some(p.rate),
            max_vus: Some(p.max_vus),
            ..BenchProfile::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{ArrivalRate, ConstantVus, GRACEFUL_STOP, RampingVus, Stage};

    #[test]
    fn profiles_show_their_flags() {
        let constant = Profile::ConstantVus(ConstantVus {
            vus: 3,
            limit: Limit::Iterations(10),
            think: ThinkTime::Range(Duration::from_secs(1), Duration::from_secs(3)),
        });
        let shown = profile(&constant);
        assert_eq!(shown.executor, "constant-vus");
        assert_eq!((shown.vus, shown.iterations), (Some(3), Some(10)));
        assert_eq!(shown.think_time.as_deref(), Some("1s..3s"));
        let ramping = Profile::RampingVus(RampingVus {
            stages: vec![Stage {
                duration: Duration::from_secs(30),
                target: 20,
            }],
            think: ThinkTime::None,
            graceful_stop: GRACEFUL_STOP,
        });
        let shown = profile(&ramping);
        assert_eq!(shown.duration_ms, Some(30_000));
        assert_eq!(shown.stages[0].target, 20);
        let open = Profile::ConstantArrivalRate(ArrivalRate {
            rate: 10,
            duration: Duration::from_secs(5),
            max_vus: 4,
        });
        let shown = profile(&open);
        assert_eq!((shown.rate, shown.max_vus), (Some(10), Some(4)));
    }
}
