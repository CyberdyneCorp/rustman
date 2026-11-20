//! The load caps of `rustman.toml [stress]` (stress-testing "Stress caps"):
//! a profile asking for more requests per second or a longer run than the
//! caps allow is refused before anything runs (exit 2), naming the cap and
//! the table that raises it. Flags never raise caps.
//!
//! Rates count iterations per second; an iteration sends every request of
//! the plan, so a rate asks for `rate × requests` requests per second.

use std::num::NonZeroU32;
use std::time::Duration;

use rustman_core::stress::StressSettings;

use crate::error::BenchError;
use crate::profile::format_duration;
use crate::stress::{FlatSpec, ModeSpec, format_rate};

/// Refuse `mode` when it exceeds a cap of `caps`, with `requests` requests
/// per iteration.
pub fn check(mode: &ModeSpec, caps: &StressSettings, requests: usize) -> Result<(), BenchError> {
    let requests = requests.max(1) as f64;
    match mode {
        ModeSpec::Breakpoint(b) => {
            b.validate()?;
            check_rate("--start-rps", b.start_rps, requests, caps)?;
            check_step(b.step_duration, caps.max_duration)
        }
        ModeSpec::Spike(s) => {
            s.validate()?;
            check_rate("the spike rate", s.peak_rps(), requests, caps)?;
            check_duration(s.total(), "max_duration", caps.max_duration)
        }
        ModeSpec::Soak(s) => {
            check_flat(&s.flat())?;
            check_rate("--rps", s.rps, requests, caps)?;
            check_duration(s.duration, "max_soak_duration", caps.max_soak_duration)
        }
        ModeSpec::Fuzz(f) => {
            check_flat(f)?;
            check_rate("--rps", f.rps, requests, caps)?;
            check_duration(f.duration, "max_duration", caps.max_duration)
        }
    }
}

/// The VU pool: `--max-vus` (`asked`), refused above the `max_vus` cap;
/// by default `max_rps`, within that cap.
pub fn pool(asked: Option<u32>, caps: &StressSettings) -> Result<u32, BenchError> {
    let cap = caps.max_vus.get();
    match asked {
        Some(0) => Err(BenchError::Usage(
            "stress: --max-vus must be above 0".into(),
        )),
        Some(vus) if vus > cap => Err(over(
            &format!("--max-vus {vus}"),
            "max_vus",
            &cap.to_string(),
        )),
        Some(vus) => Ok(vus),
        None => Ok(caps.max_rps.get().min(cap)),
    }
}

/// The governor's burst under `max_rps`: a tenth of a second of requests,
/// at least two. The scheduler offers at most `max_rps` on average; with
/// no burst, every request that arrives a little early for its slot waits
/// and a late one earns nothing back, so offering exactly the cap would
/// queue requests client-side and read as server latency.
pub fn governor_burst(max_rps: NonZeroU32) -> NonZeroU32 {
    let tenth = max_rps.get().div_ceil(10).max(2);
    NonZeroU32::new(tenth).unwrap_or(NonZeroU32::MIN)
}

/// The highest iteration rate the caps allow.
pub fn max_rate(caps: &StressSettings, requests: usize) -> f64 {
    f64::from(caps.max_rps.get()) / requests.max(1) as f64
}

/// How long load may run at most.
pub fn run_time(mode: &ModeSpec, caps: &StressSettings) -> Duration {
    match mode {
        ModeSpec::Soak(_) => caps.max_soak_duration,
        _ => caps.max_duration,
    }
}

fn check_flat(flat: &FlatSpec) -> Result<(), BenchError> {
    if !(flat.rps.is_finite() && flat.rps > 0.0) {
        return Err(BenchError::Usage("stress: --rps must be above 0".into()));
    }
    Ok(())
}

fn check_rate(
    what: &str,
    rate: f64,
    requests: f64,
    caps: &StressSettings,
) -> Result<(), BenchError> {
    let rps = rate * requests;
    if rps <= f64::from(caps.max_rps.get()) {
        return Ok(());
    }
    let asked = if requests > 1.0 {
        format!(
            "{what} {} with {requests} requests per iteration ({} requests/s)",
            format_rate(rate),
            format_rate(rps)
        )
    } else {
        format!("{what} {}", format_rate(rate))
    };
    Err(over(&asked, "max_rps", &caps.max_rps.to_string()))
}

/// A breakpoint step longer than the run may last would be cut short as
/// the only step, with no search at all.
fn check_step(step: Duration, cap: Duration) -> Result<(), BenchError> {
    if step <= cap {
        return Ok(());
    }
    let asked = format!("--step-duration {}", format_long(step));
    Err(over(&asked, "max_duration", &format_long(cap)))
}

fn check_duration(duration: Duration, key: &str, cap: Duration) -> Result<(), BenchError> {
    if duration <= cap {
        return Ok(());
    }
    let asked = format!("a duration of {}", format_long(duration));
    Err(over(&asked, key, &format_long(cap)))
}

fn over(asked: &str, key: &str, cap: &str) -> BenchError {
    crate::caps::exceeds("stress", asked, key, cap)
}

/// `2h`, `90m`, `30s`: hours when whole, else the bench's format.
pub fn format_long(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs >= 3600 && secs.is_multiple_of(3600) && duration.subsec_nanos() == 0 {
        format!("{}h", secs / 3600)
    } else {
        format_duration(duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_governor_burst_is_a_tenth_of_a_second_of_the_cap() {
        let burst = |rps: u32| governor_burst(NonZeroU32::new(rps).unwrap()).get();
        assert_eq!(burst(1), 2);
        assert_eq!(burst(30), 3);
        assert_eq!(burst(500), 50);
    }

    #[test]
    fn the_vu_pool_never_exceeds_the_max_vus_cap() {
        let mut caps = StressSettings::default();
        assert_eq!(pool(None, &caps).unwrap(), 500);
        assert_eq!(pool(Some(64), &caps).unwrap(), 64);
        assert_eq!(
            pool(Some(1001), &caps).unwrap_err().to_string(),
            "stress: --max-vus 1001 exceeds max_vus = 1000 (raise it in rustman.toml [stress])"
        );
        assert!(pool(Some(0), &caps).is_err());
        caps.max_vus = NonZeroU32::new(100).unwrap();
        assert_eq!(pool(None, &caps).unwrap(), 100);
        caps.max_vus = NonZeroU32::new(5000).unwrap();
        assert_eq!(pool(Some(5000), &caps).unwrap(), 5000);
    }
    use crate::stress::{BreakpointSpec, SoakSpec, SpikeSpec};

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_breakpoint_step_longer_than_max_duration_is_refused() {
        let step = |d| {
            ModeSpec::Breakpoint(BreakpointSpec {
                step_duration: d,
                ..BreakpointSpec::default()
            })
        };
        let caps = StressSettings::default();
        assert_eq!(
            check(&step(secs(20 * 60)), &caps, 1)
                .unwrap_err()
                .to_string(),
            "stress: --step-duration 20m exceeds max_duration = 10m (raise it in rustman.toml [stress])"
        );
        assert!(check(&step(secs(10 * 60)), &caps, 1).is_ok());
    }

    #[test]
    fn a_soak_over_its_cap_names_max_soak_duration() {
        let soak = ModeSpec::Soak(SoakSpec {
            rps: 10.0,
            duration: secs(4 * 3600),
            ..SoakSpec::default()
        });
        let err = check(&soak, &StressSettings::default(), 1).unwrap_err();
        assert!(!err.is_refusal());
        assert_eq!(
            err.to_string(),
            "stress: a duration of 4h exceeds max_soak_duration = 2h (raise it in rustman.toml [stress])"
        );
        let ok = ModeSpec::Soak(SoakSpec {
            rps: 10.0,
            duration: secs(2 * 3600),
            ..SoakSpec::default()
        });
        assert!(check(&ok, &StressSettings::default(), 1).is_ok());
    }

    #[test]
    fn rates_count_every_request_of_an_iteration() {
        let start = |rps| {
            ModeSpec::Breakpoint(BreakpointSpec {
                start_rps: rps,
                ..BreakpointSpec::default()
            })
        };
        let caps = StressSettings::default();
        assert!(check(&start(500.0), &caps, 1).is_ok());
        assert_eq!(
            check(&start(600.0), &caps, 1).unwrap_err().to_string(),
            "stress: --start-rps 600 exceeds max_rps = 500 (raise it in rustman.toml [stress])"
        );
        assert!(
            check(&start(300.0), &caps, 2)
                .unwrap_err()
                .to_string()
                .contains("(600 requests/s)")
        );
        let raised = StressSettings {
            max_rps: NonZeroU32::new(1000).unwrap(),
            ..caps
        };
        assert!(check(&start(600.0), &raised, 1).is_ok());
        assert_eq!(max_rate(&raised, 4), 250.0);
    }

    #[test]
    fn a_spike_checks_its_peak_and_its_length() {
        let spike = |baseline_rps, spike_duration| {
            ModeSpec::Spike(SpikeSpec {
                baseline_rps,
                spike_duration,
                ..SpikeSpec::default()
            })
        };
        let caps = StressSettings::default();
        assert!(check(&spike(50.0, secs(30)), &caps, 1).is_ok());
        assert!(
            check(&spike(60.0, secs(30)), &caps, 1)
                .unwrap_err()
                .to_string()
                .contains("the spike rate 600 exceeds max_rps = 500")
        );
        assert!(
            check(&spike(10.0, secs(600)), &caps, 1)
                .unwrap_err()
                .to_string()
                .contains("exceeds max_duration = 10m")
        );
    }

    #[test]
    fn a_fuzz_run_checks_its_rate_and_duration() {
        let fuzz = ModeSpec::Fuzz(FlatSpec {
            rps: 0.0,
            duration: secs(10),
        });
        assert!(check(&fuzz, &StressSettings::default(), 1).is_err());
        let long = ModeSpec::Fuzz(FlatSpec {
            rps: 5.0,
            duration: secs(3600),
        });
        assert!(
            check(&long, &StressSettings::default(), 1)
                .unwrap_err()
                .to_string()
                .contains("a duration of 1h exceeds max_duration = 10m")
        );
    }

    #[test]
    fn the_run_time_is_the_mode_cap() {
        let caps = StressSettings::default();
        let soak = ModeSpec::Soak(SoakSpec::default());
        assert_eq!(run_time(&soak, &caps), secs(7200));
        let bp = ModeSpec::Breakpoint(BreakpointSpec::default());
        assert_eq!(run_time(&bp, &caps), secs(600));
        assert_eq!(format_long(secs(5400)), "90m");
    }
}
