//! The load caps of `rustman.toml [bench]` (load-testing "Default load
//! caps"): a profile asking for more VUs, time or requests per second than
//! the caps allow is refused before anything runs. Flags never raise caps.

use std::time::Duration;

use rustman_core::bench::BenchSettings;

use crate::error::BenchError;
use crate::profile::{Profile, format_duration};

/// Refuse `profile` when it exceeds a cap of `caps`, naming the cap and the
/// `[bench]` key that raises it. An iteration sends `requests` requests, so
/// an arrival rate asks for `rate × requests` requests per second.
pub fn check(profile: &Profile, caps: &BenchSettings, requests: usize) -> Result<(), BenchError> {
    let vus = profile.max_vus();
    if vus > caps.max_vus.get() {
        return Err(over(
            format!("{vus} VUs"),
            "max_vus",
            caps.max_vus.to_string(),
        ));
    }
    if let Some(duration) = profile.duration().filter(|d| *d > caps.max_duration) {
        return Err(over(
            format!("a duration of {}", format_duration(duration)),
            "max_duration",
            format_duration(caps.max_duration),
        ));
    }
    if let Profile::ConstantArrivalRate(open) = profile {
        check_rate(open.rate, requests, caps)?;
    }
    Ok(())
}

fn check_rate(rate: u32, requests: usize, caps: &BenchSettings) -> Result<(), BenchError> {
    let requests = u64::try_from(requests.max(1)).unwrap_or(u64::MAX);
    let rps = u64::from(rate).saturating_mul(requests);
    if rps <= u64::from(caps.max_rps.get()) {
        return Ok(());
    }
    let asked = if requests == 1 {
        format!("a rate of {rate}/s")
    } else {
        format!("a rate of {rate}/s with {requests} requests per iteration ({rps} requests/s)")
    };
    Err(over(asked, "max_rps", caps.max_rps.to_string()))
}

/// How long load may be generated: the profile's duration, else (an
/// iterations limit) the `max_duration` cap.
pub fn run_time(profile: &Profile, caps: &BenchSettings) -> Duration {
    profile
        .duration()
        .unwrap_or(caps.max_duration)
        .min(caps.max_duration)
}

fn over(asked: String, key: &str, cap: String) -> BenchError {
    BenchError::Usage(format!(
        "bench: {asked} exceeds {key} = {cap} (raise it in rustman.toml [bench])"
    ))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;
    use crate::profile::{ArrivalRate, ConstantVus, Limit, RampingVus, ThinkTime};

    fn constant(vus: u32, limit: Limit) -> Profile {
        Profile::ConstantVus(ConstantVus {
            vus,
            limit,
            think: ThinkTime::None,
        })
    }

    fn message(profile: &Profile, caps: &BenchSettings) -> String {
        check(profile, caps, 1).unwrap_err().to_string()
    }

    #[test]
    fn the_vu_cap_message_names_the_key() {
        let profile = constant(100, Limit::Duration(Duration::from_secs(30)));
        assert_eq!(
            message(&profile, &BenchSettings::default()),
            "bench: 100 VUs exceeds max_vus = 50 (raise it in rustman.toml [bench])"
        );
        let raised = BenchSettings {
            max_vus: NonZeroU32::new(200).unwrap(),
            ..BenchSettings::default()
        };
        assert!(check(&profile, &raised, 1).is_ok());
    }

    #[test]
    fn duration_and_rate_caps_apply() {
        let long = constant(1, Limit::Duration(Duration::from_secs(600)));
        assert_eq!(
            message(&long, &BenchSettings::default()),
            "bench: a duration of 10m exceeds max_duration = 5m (raise it in rustman.toml [bench])"
        );
        let fast = Profile::ConstantArrivalRate(ArrivalRate {
            rate: 500,
            duration: Duration::from_secs(10),
            max_vus: 10,
        });
        assert_eq!(
            message(&fast, &BenchSettings::default()),
            "bench: a rate of 500/s exceeds max_rps = 200 (raise it in rustman.toml [bench])"
        );
        let ramp = Profile::RampingVus(RampingVus {
            stages: vec!["4m:10".parse().unwrap(), "2m:0".parse().unwrap()],
            think: ThinkTime::None,
            graceful_stop: Duration::from_secs(30),
        });
        assert!(message(&ramp, &BenchSettings::default()).contains("max_duration = 5m"));
    }

    #[test]
    fn an_iterations_limit_runs_at_most_max_duration() {
        let profile = constant(2, Limit::Iterations(10));
        let caps = BenchSettings::default();
        assert!(check(&profile, &caps, 1).is_ok());
        assert_eq!(run_time(&profile, &caps), caps.max_duration);
        let short = constant(2, Limit::Duration(Duration::from_secs(3)));
        assert_eq!(run_time(&short, &caps), Duration::from_secs(3));
    }

    #[test]
    fn the_rate_cap_counts_every_request_of_an_iteration() {
        let open = Profile::ConstantArrivalRate(ArrivalRate {
            rate: 150,
            duration: Duration::from_secs(10),
            max_vus: 10,
        });
        let caps = BenchSettings::default();
        assert!(check(&open, &caps, 1).is_ok());
        assert_eq!(
            check(&open, &caps, 2).unwrap_err().to_string(),
            "bench: a rate of 150/s with 2 requests per iteration (300 requests/s) exceeds \
             max_rps = 200 (raise it in rustman.toml [bench])"
        );
    }
}
