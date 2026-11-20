//! Load profiles: which executor runs the virtual users, for how long, and
//! how they pause between iterations.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use rustman_core::bench::{BenchSettings, BenchStage};
use rustman_core::duration::parse_duration;

use crate::error::BenchError;

/// Default `--duration`.
pub const DEFAULT_DURATION: Duration = Duration::from_secs(30);
/// Default `--max-vus` of the arrival-rate executor.
pub const DEFAULT_MAX_VUS: u32 = 50;
/// How long VUs removed by a ramp-down, or still running when the profile
/// ends, may take to finish their iteration before it is cancelled.
pub const GRACEFUL_STOP: Duration = Duration::from_secs(30);

/// A load profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Profile {
    ConstantVus(ConstantVus),
    RampingVus(RampingVus),
    ConstantArrivalRate(ArrivalRate),
}

/// `constant-vus`: a fixed number of VUs in a closed loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstantVus {
    pub vus: u32,
    pub limit: Limit,
    pub think: ThinkTime,
}

/// What ends a `constant-vus` run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    Duration(Duration),
    /// Iterations in total, across every VU.
    Iterations(u64),
}

/// `ramping-vus`: the VU count follows `stages`, starting from 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RampingVus {
    pub stages: Vec<Stage>,
    pub think: ThinkTime,
    pub graceful_stop: Duration,
}

/// One ramping stage: move linearly to `target` VUs over `duration`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stage {
    pub duration: Duration,
    pub target: u32,
}

/// `constant-arrival-rate`: `rate` iterations start every second (an open
/// model), each on a free VU of a pool of `max_vus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArrivalRate {
    pub rate: u32,
    pub duration: Duration,
    pub max_vus: u32,
}

/// The pause of a VU between two iterations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThinkTime {
    #[default]
    None,
    Fixed(Duration),
    /// Uniformly random between the bounds (inclusive).
    Range(Duration, Duration),
}

impl Profile {
    /// The most VUs the profile runs at once: the size of the VU pool.
    pub fn max_vus(&self) -> u32 {
        match self {
            Self::ConstantVus(p) => p.vus,
            Self::RampingVus(p) => p.stages.iter().map(|s| s.target).max().unwrap_or(0),
            Self::ConstantArrivalRate(p) => p.max_vus,
        }
    }

    /// How long load is generated, when the profile says (an iterations
    /// limit does not).
    pub fn duration(&self) -> Option<Duration> {
        match self {
            Self::ConstantVus(p) => match p.limit {
                Limit::Duration(d) => Some(d),
                Limit::Iterations(_) => None,
            },
            Self::RampingVus(p) => Some(p.stages.iter().map(|s| s.duration).sum()),
            Self::ConstantArrivalRate(p) => Some(p.duration),
        }
    }

    /// How long iterations still running when the profile ends may take.
    pub fn graceful_stop(&self) -> Duration {
        match self {
            Self::RampingVus(p) => p.graceful_stop,
            _ => GRACEFUL_STOP,
        }
    }

    /// Refuse a profile that cannot run: no VU, no stage, no rate.
    pub fn validate(&self) -> Result<(), BenchError> {
        let problem = match self {
            Self::RampingVus(p) if p.stages.is_empty() => {
                Some("ramping-vus needs at least one --stage <duration>:<vus> or [[bench.stages]]")
            }
            Self::RampingVus(_) if self.max_vus() == 0 => {
                Some("ramping-vus needs a stage with at least one VU")
            }
            Self::ConstantArrivalRate(p) if p.rate == 0 => Some("--rate must be at least 1"),
            Self::ConstantVus(ConstantVus {
                limit: Limit::Iterations(0),
                ..
            }) => Some("--iterations must be at least 1"),
            _ if self.max_vus() == 0 => Some("at least one VU is needed"),
            _ => None,
        };
        match problem {
            Some(problem) => Err(BenchError::Usage(format!("bench: {problem}"))),
            None => Ok(()),
        }
    }

    /// The executor's name, as `--executor` spells it.
    pub fn executor(&self) -> ExecutorKind {
        match self {
            Self::ConstantVus(_) => ExecutorKind::ConstantVus,
            Self::RampingVus(_) => ExecutorKind::RampingVus,
            Self::ConstantArrivalRate(_) => ExecutorKind::ConstantArrivalRate,
        }
    }
}

/// The executors `--executor` names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExecutorKind {
    #[default]
    ConstantVus,
    RampingVus,
    ConstantArrivalRate,
}

impl ExecutorKind {
    pub const ALL: [Self; 3] = [
        Self::ConstantVus,
        Self::RampingVus,
        Self::ConstantArrivalRate,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::ConstantVus => "constant-vus",
            Self::RampingVus => "ramping-vus",
            Self::ConstantArrivalRate => "constant-arrival-rate",
        }
    }
}

impl fmt::Display for ExecutorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for ExecutorKind {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.name() == text)
            .ok_or_else(|| {
                format!("unknown executor '{text}' (use constant-vus, ramping-vus or constant-arrival-rate)")
            })
    }
}

impl From<&BenchStage> for Stage {
    fn from(stage: &BenchStage) -> Self {
        Self {
            duration: stage.duration,
            target: stage.target,
        }
    }
}

impl FromStr for Stage {
    type Err = String;

    /// `<duration>:<target_vus>`, as in `30s:20`.
    fn from_str(text: &str) -> Result<Self, String> {
        let invalid = || format!("invalid stage '{text}' (use <duration>:<vus>, as in 30s:20)");
        let (duration, target) = text.split_once(':').ok_or_else(invalid)?;
        Ok(Self {
            duration: parse_duration(duration).map_err(|_| invalid())?,
            target: target.trim().parse().map_err(|_| invalid())?,
        })
    }
}

impl FromStr for ThinkTime {
    type Err = String;

    /// `0`, a duration (`500ms`), or a `min..max` range (`1s..3s`).
    fn from_str(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let Some((min, max)) = text.split_once("..") else {
            return Ok(match think_duration(text)? {
                d if d.is_zero() => Self::None,
                d => Self::Fixed(d),
            });
        };
        let (min, max) = (think_duration(min)?, think_duration(max)?);
        if min > max {
            return Err(format!(
                "invalid think time '{text}': the minimum is above the maximum"
            ));
        }
        Ok(Self::Range(min, max))
    }
}

/// A think-time bound: a duration, or `0`.
fn think_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    if text == "0" {
        return Ok(Duration::ZERO);
    }
    parse_duration(text).map_err(|e| format!("invalid think time: {e}"))
}

impl ThinkTime {
    /// The pause before a VU's next iteration.
    pub fn pause(self) -> Duration {
        match self {
            Self::None => Duration::ZERO,
            Self::Fixed(d) => d,
            Self::Range(min, max) => {
                let (min, max) = (min.as_micros(), max.as_micros());
                let micros = rand::random_range(min..=max);
                Duration::from_micros(u64::try_from(micros).unwrap_or(u64::MAX))
            }
        }
    }
}

/// A profile as flags describe it (`--executor`, `--vus`, `--duration`,
/// `--iterations`, `--think-time`, `--stage`, `--rate`, `--max-vus`), before
/// it is checked: each flag applies to some executors only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileSpec {
    pub executor: ExecutorKind,
    pub vus: Option<u32>,
    pub duration: Option<Duration>,
    pub iterations: Option<u64>,
    pub think: ThinkTime,
    pub stages: Vec<Stage>,
    pub rate: Option<u32>,
    pub max_vus: Option<u32>,
}

impl ProfileSpec {
    /// For `ramping-vus` without `--stage`, the `[[bench.stages]]` of
    /// `rustman.toml`. `--stage` flags replace them entirely.
    #[must_use]
    pub fn or_workspace_stages(mut self, settings: &BenchSettings) -> Self {
        if self.executor == ExecutorKind::RampingVus && self.stages.is_empty() {
            self.stages = settings.stages.iter().map(Stage::from).collect();
        }
        self
    }

    /// The profile, or a usage error naming a flag the executor doesn't
    /// take. `--max-vus` defaults to 50, or to `max_vus` when that is lower.
    pub fn build(&self, max_vus_cap: u32) -> Result<Profile, BenchError> {
        self.check_flags()?;
        let duration = self.duration.unwrap_or(DEFAULT_DURATION);
        Ok(match self.executor {
            ExecutorKind::ConstantVus => Profile::ConstantVus(ConstantVus {
                vus: self.vus.unwrap_or(1),
                limit: self
                    .iterations
                    .map_or(Limit::Duration(duration), Limit::Iterations),
                think: self.think,
            }),
            ExecutorKind::RampingVus => Profile::RampingVus(RampingVus {
                stages: self.stages.clone(),
                think: self.think,
                graceful_stop: GRACEFUL_STOP,
            }),
            ExecutorKind::ConstantArrivalRate => Profile::ConstantArrivalRate(ArrivalRate {
                rate: self.rate.ok_or_else(|| {
                    BenchError::Usage("bench: constant-arrival-rate needs --rate".into())
                })?,
                duration,
                max_vus: self.max_vus.unwrap_or(DEFAULT_MAX_VUS.min(max_vus_cap)),
            }),
        })
    }

    /// Refuse a flag the executor ignores.
    fn check_flags(&self) -> Result<(), BenchError> {
        use ExecutorKind::{
            ConstantArrivalRate as Open, ConstantVus as Constant, RampingVus as Ramp,
        };
        let flags: [(&str, bool, &[ExecutorKind]); 6] = [
            ("--vus", self.vus.is_some(), &[Constant]),
            ("--duration", self.duration.is_some(), &[Constant, Open]),
            ("--iterations", self.iterations.is_some(), &[Constant]),
            (
                "--think-time",
                self.think != ThinkTime::None,
                &[Constant, Ramp],
            ),
            ("--stage", !self.stages.is_empty(), &[Ramp]),
            ("--rate", self.rate.is_some(), &[Open]),
        ];
        let max_vus = ("--max-vus", self.max_vus.is_some(), &[Open][..]);
        let misplaced = flags
            .into_iter()
            .chain([max_vus])
            .find(|(_, given, executors)| *given && !executors.contains(&self.executor));
        match misplaced {
            Some((flag, _, _)) => Err(BenchError::Usage(format!(
                "bench: {flag} does not apply to the {} executor",
                self.executor
            ))),
            None => Ok(()),
        }
    }
}

/// The VU count a ramping profile asks for `elapsed` after its start: a
/// linear move from the previous stage's target (0 before the first) to the
/// stage's own, rounded to the nearest VU. After the last stage, its target.
pub fn ramp_target(stages: &[Stage], elapsed: Duration) -> u32 {
    let mut from = 0u32;
    let mut stage_start = Duration::ZERO;
    for stage in stages {
        let stage_end = stage_start + stage.duration;
        if elapsed < stage_end {
            let progress = (elapsed - stage_start).as_secs_f64() / stage.duration.as_secs_f64();
            let delta = f64::from(stage.target) - f64::from(from);
            let vus = (f64::from(from) + delta * progress).round();
            // Between two u32 targets, so it fits.
            return vus as u32;
        }
        from = stage.target;
        stage_start = stage_end;
    }
    from
}

/// `duration` the way users write it: `500ms`, `30s` or `5m`.
pub fn format_duration(duration: Duration) -> String {
    let ms = duration.as_millis();
    if ms.is_multiple_of(60_000) {
        format!("{}m", ms / 60_000)
    } else if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{ms}ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn stages_parse() {
        assert_eq!(
            "30s:20".parse::<Stage>().unwrap(),
            Stage {
                duration: secs(30),
                target: 20
            }
        );
        assert_eq!("1m:0".parse::<Stage>().unwrap().duration, secs(60));
        for bad in ["30s", "x:1", "30s:-1", "0s:3"] {
            let err = bad.parse::<Stage>().unwrap_err();
            assert!(err.contains("invalid stage"), "{err}");
        }
    }

    #[test]
    fn think_times_parse() {
        assert_eq!("0".parse::<ThinkTime>().unwrap(), ThinkTime::None);
        assert_eq!(
            "500ms".parse::<ThinkTime>().unwrap(),
            ThinkTime::Fixed(Duration::from_millis(500))
        );
        assert_eq!(
            "1s..3s".parse::<ThinkTime>().unwrap(),
            ThinkTime::Range(secs(1), secs(3))
        );
        assert!("3s..1s".parse::<ThinkTime>().is_err());
        assert!("soon".parse::<ThinkTime>().is_err());
    }

    #[test]
    fn ranged_think_time_stays_in_its_bounds() {
        let think = ThinkTime::Range(secs(1), secs(3));
        for _ in 0..200 {
            let pause = think.pause();
            assert!(pause >= secs(1) && pause <= secs(3), "{pause:?}");
        }
        assert_eq!(ThinkTime::Fixed(secs(2)).pause(), secs(2));
        assert_eq!(ThinkTime::None.pause(), Duration::ZERO);
    }

    #[test]
    fn ramps_are_linear_and_hold_the_last_target() {
        let stages: Vec<Stage> = ["30s:20", "1m:20", "30s:0"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(ramp_target(&stages, Duration::ZERO), 0);
        assert_eq!(ramp_target(&stages, secs(15)), 10);
        assert_eq!(ramp_target(&stages, secs(30)), 20);
        assert_eq!(ramp_target(&stages, secs(60)), 20);
        assert_eq!(ramp_target(&stages, secs(105)), 10);
        assert_eq!(ramp_target(&stages, secs(120)), 0);
        assert_eq!(ramp_target(&stages, secs(500)), 0);
    }

    #[test]
    fn profiles_know_their_pool_and_duration() {
        let ramp = Profile::RampingVus(RampingVus {
            stages: vec!["10s:5".parse().unwrap(), "20s:2".parse().unwrap()],
            think: ThinkTime::None,
            graceful_stop: GRACEFUL_STOP,
        });
        assert_eq!(ramp.max_vus(), 5);
        assert_eq!(ramp.duration(), Some(secs(30)));
        let iterations = Profile::ConstantVus(ConstantVus {
            vus: 3,
            limit: Limit::Iterations(9),
            think: ThinkTime::None,
        });
        assert_eq!(iterations.duration(), None);
        assert_eq!(iterations.executor().name(), "constant-vus");
    }

    #[test]
    fn profiles_that_cannot_run_are_refused() {
        let no_stage = Profile::RampingVus(RampingVus {
            stages: Vec::new(),
            think: ThinkTime::None,
            graceful_stop: GRACEFUL_STOP,
        });
        assert!(
            no_stage
                .validate()
                .unwrap_err()
                .to_string()
                .contains("--stage")
        );
        let idle = Profile::RampingVus(RampingVus {
            stages: vec!["1s:0".parse().unwrap()],
            think: ThinkTime::None,
            graceful_stop: GRACEFUL_STOP,
        });
        assert!(idle.validate().is_err());
        let no_rate = Profile::ConstantArrivalRate(ArrivalRate {
            rate: 0,
            duration: secs(1),
            max_vus: 1,
        });
        assert!(no_rate.validate().is_err());
        let no_vu = Profile::ConstantVus(ConstantVus {
            vus: 0,
            limit: Limit::Duration(secs(1)),
            think: ThinkTime::None,
        });
        assert!(no_vu.validate().is_err());
        let fine = Profile::ConstantVus(ConstantVus {
            vus: 1,
            limit: Limit::Iterations(1),
            think: ThinkTime::None,
        });
        assert!(fine.validate().is_ok());
    }

    #[test]
    fn flags_build_each_executor() {
        let constant = ProfileSpec {
            vus: Some(3),
            iterations: Some(9),
            ..ProfileSpec::default()
        };
        assert_eq!(
            constant.build(50).unwrap(),
            Profile::ConstantVus(ConstantVus {
                vus: 3,
                limit: Limit::Iterations(9),
                think: ThinkTime::None,
            })
        );
        let default = ProfileSpec::default().build(50).unwrap();
        assert_eq!(default.duration(), Some(DEFAULT_DURATION));
        assert_eq!(default.max_vus(), 1);
        let open = ProfileSpec {
            executor: ExecutorKind::ConstantArrivalRate,
            rate: Some(10),
            ..ProfileSpec::default()
        };
        assert_eq!(open.build(20).unwrap().max_vus(), 20);
        assert_eq!(open.build(500).unwrap().max_vus(), DEFAULT_MAX_VUS);
        let ramp = ProfileSpec {
            executor: ExecutorKind::RampingVus,
            stages: vec!["1s:2".parse().unwrap()],
            ..ProfileSpec::default()
        };
        assert_eq!(ramp.build(50).unwrap().max_vus(), 2);
    }

    #[test]
    fn flags_of_other_executors_are_refused() {
        let ramp_with_vus = ProfileSpec {
            executor: ExecutorKind::RampingVus,
            vus: Some(3),
            ..ProfileSpec::default()
        };
        assert_eq!(
            ramp_with_vus.build(50).unwrap_err().to_string(),
            "bench: --vus does not apply to the ramping-vus executor"
        );
        let stage_without_ramp = ProfileSpec {
            stages: vec!["1s:2".parse().unwrap()],
            ..ProfileSpec::default()
        };
        assert!(stage_without_ramp.build(50).is_err());
        let open_without_rate = ProfileSpec {
            executor: ExecutorKind::ConstantArrivalRate,
            ..ProfileSpec::default()
        };
        assert_eq!(
            open_without_rate.build(50).unwrap_err().to_string(),
            "bench: constant-arrival-rate needs --rate"
        );
        let max_vus_closed = ProfileSpec {
            max_vus: Some(3),
            ..ProfileSpec::default()
        };
        assert!(max_vus_closed.build(50).is_err());
    }

    #[test]
    fn workspace_stages_apply_to_ramping_vus_without_a_stage_flag() {
        let settings = BenchSettings {
            stages: vec![BenchStage {
                duration: secs(2),
                target: 3,
            }],
            ..BenchSettings::default()
        };
        let ramp = ProfileSpec {
            executor: ExecutorKind::RampingVus,
            ..ProfileSpec::default()
        };
        let from_settings = ramp.clone().or_workspace_stages(&settings);
        assert_eq!(from_settings.stages, ["2s:3".parse().unwrap()]);
        assert_eq!(from_settings.build(50).unwrap().max_vus(), 3);
        let flagged = ProfileSpec {
            stages: vec!["1s:5".parse().unwrap()],
            ..ramp
        };
        assert_eq!(
            flagged.clone().or_workspace_stages(&settings).stages,
            flagged.stages
        );
        // Other executors never take them (a --stage would be refused).
        let constant = ProfileSpec::default().or_workspace_stages(&settings);
        assert!(constant.stages.is_empty());
        assert!(constant.build(50).is_ok());
    }

    #[test]
    fn executors_parse_by_name() {
        for kind in ExecutorKind::ALL {
            assert_eq!(kind.name().parse::<ExecutorKind>().unwrap(), kind);
        }
        assert!("shared-iterations".parse::<ExecutorKind>().is_err());
    }

    #[test]
    fn durations_print_as_written() {
        assert_eq!(format_duration(secs(300)), "5m");
        assert_eq!(format_duration(secs(90)), "90s");
        assert_eq!(format_duration(Duration::from_millis(1500)), "1500ms");
    }
}
