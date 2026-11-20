//! The stress gate (stress-testing "Stress targets come only from config",
//! "Stress production guard", "Costly families need config opt-in",
//! "Wire-fault families"): the rules a stress run adds on top of the shared
//! safety policy. It runs after [`crate::safety::policy`] built and checked
//! the policy pinned to the target, and only adds refusals:
//!
//! 1. the target must be listed in `rustman.toml [stress] targets`;
//! 2. a production target (as the shared production guard decides, which
//!    already required `--production <host>`) must also be listed in
//!    `[stress] allow_production`;
//! 3. fuzz mixing, malformed families and wire faults never go to
//!    production;
//! 4. `compression-bomb` runs only when listed in `[stress] families`;
//! 5. wire faults need `--allow-wire-faults`.
//!
//! Every refusal is a [`BenchError::Refused`] (exit 4), checked before
//! anything is sent; unknown family names and a fuzz ratio outside
//! `0 < r <= 1` are usage errors (exit 2).

use rustman_core::safety::{Origin, SafetyPolicy};
use rustman_core::stress::{MALFORMED_FAMILIES, StressSettings, WIRE_FAMILIES};

use crate::error::BenchError;
use crate::stress::wire::{WireConsent, WireFamily};

/// What a stress run asks for beyond its load profile.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ask<'a> {
    /// `--fuzz-ratio`; fuzz mode defaults it.
    pub fuzz_ratio: Option<f64>,
    /// `--malformed` families.
    pub malformed: &'a [String],
    /// `--wire` families.
    pub wire: &'a [String],
    /// `--allow-wire-faults`.
    pub allow_wire_faults: bool,
}

impl Ask<'_> {
    /// Whether the run sends anything but the saved, valid requests.
    pub fn sends_bad_input(&self) -> bool {
        self.fuzz_ratio.is_some() || !self.malformed.is_empty() || !self.wire.is_empty()
    }
}

/// The message of an unlisted target.
pub const UNLISTED_TARGET: &str = "stress: target not listed in rustman.toml [stress].targets";
/// The message of bad input against production.
pub const NO_BAD_INPUT_IN_PRODUCTION: &str =
    "stress: fuzzing and malformed input are never sent to production";

/// Check `ask` and the policy's target against the stress rules.
pub fn check(
    settings: &StressSettings,
    policy: &SafetyPolicy,
    ask: &Ask<'_>,
) -> Result<(), BenchError> {
    check_usage(ask)?;
    check_target(settings, policy.target())?;
    if policy.production().is_some() {
        check_production(settings, policy.target(), ask)?;
    }
    check_opt_ins(settings, ask)
}

/// The permission to send `ask`'s wire faults to the policy's target, once
/// [`check`] passed: `None` when no wire fault was asked for.
pub fn wire_consent(
    settings: &StressSettings,
    policy: &SafetyPolicy,
    ask: &Ask<'_>,
) -> Result<Option<WireConsent>, BenchError> {
    check(settings, policy, ask)?;
    if ask.wire.is_empty() {
        return Ok(None);
    }
    let families = ask
        .wire
        .iter()
        .map(|f| f.parse::<WireFamily>().map_err(BenchError::Usage))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(WireConsent::new(
        policy.target().clone(),
        families,
        settings.max_wire_connections.get(),
    )))
}

/// Unknown family names and a fuzz ratio outside `0 < r <= 1`.
fn check_usage(ask: &Ask<'_>) -> Result<(), BenchError> {
    if let Some(ratio) = ask.fuzz_ratio
        && !(ratio > 0.0 && ratio <= 1.0)
    {
        return Err(BenchError::Usage(format!(
            "stress: --fuzz-ratio {ratio} is outside 0 < r <= 1"
        )));
    }
    known_families("--malformed", ask.malformed, MALFORMED_FAMILIES)?;
    known_families("--wire", ask.wire, WIRE_FAMILIES)
}

fn known_families(flag: &str, asked: &[String], known: &[&str]) -> Result<(), BenchError> {
    match asked.iter().find(|f| !known.contains(&f.as_str())) {
        Some(bad) => Err(BenchError::Usage(format!(
            "stress: {flag}: unknown family '{bad}' (expected {})",
            known.join(", ")
        ))),
        None => Ok(()),
    }
}

fn check_target(settings: &StressSettings, target: &Origin) -> Result<(), BenchError> {
    if settings.lists_target(target) {
        Ok(())
    } else {
        Err(BenchError::Refused(UNLISTED_TARGET.to_owned()))
    }
}

/// A production target the shared guard let through (`--production`
/// repeated its host): it must be in `allow_production` too, and only
/// valid requests are sent to it.
fn check_production(
    settings: &StressSettings,
    target: &Origin,
    ask: &Ask<'_>,
) -> Result<(), BenchError> {
    if !settings.allows_production(target.host()) {
        return Err(BenchError::Refused(format!(
            "stress: production host {} is not listed in rustman.toml [stress].allow_production",
            target.host()
        )));
    }
    if ask.sends_bad_input() {
        return Err(BenchError::Refused(NO_BAD_INPUT_IN_PRODUCTION.to_owned()));
    }
    Ok(())
}

fn check_opt_ins(settings: &StressSettings, ask: &Ask<'_>) -> Result<(), BenchError> {
    let bomb = "compression-bomb";
    if ask.malformed.iter().any(|f| f == bomb) && !settings.allows_family(bomb) {
        return Err(BenchError::Refused(format!(
            "stress: --malformed {bomb} needs \"{bomb}\" in rustman.toml [stress].families"
        )));
    }
    if !ask.wire.is_empty() && !ask.allow_wire_faults {
        return Err(BenchError::Refused(
            "stress: --wire sends raw, invalid HTTP and needs --allow-wire-faults".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rustman_core::safety::{
        EnvironmentInfo, HostPattern, Permission, PolicyOptions, SafetySettings,
    };

    use super::*;

    const STAGING: &str = "https://staging.example.com";
    const PROD: &str = "https://api.example.com";

    fn settings() -> StressSettings {
        StressSettings {
            targets: vec![
                Origin::parse(STAGING).unwrap(),
                Origin::parse(PROD).unwrap(),
            ],
            ..StressSettings::default()
        }
    }

    fn policy(target: &str, production: Option<&str>) -> SafetyPolicy {
        let env = EnvironmentInfo {
            name: Some(if production.is_some() {
                "prod"
            } else {
                "staging"
            }),
            marked_production: false,
        };
        let options = PolicyOptions {
            target,
            permission: Permission::ReadOnly,
            allow_hosts: &[],
            production,
            environment: env,
        };
        SafetyPolicy::new(&SafetySettings::default(), options).unwrap()
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_listed_target_with_valid_requests_passes() {
        let ok = check(&settings(), &policy(STAGING, None), &Ask::default());
        assert_eq!(ok, Ok(()));
    }

    #[test]
    fn an_unlisted_target_is_refused() {
        let other = policy("https://api.other.com", None);
        let err = check(&settings(), &other, &Ask::default()).unwrap_err();
        assert!(err.is_refusal());
        assert_eq!(err.to_string(), UNLISTED_TARGET);
    }

    #[test]
    fn production_needs_allow_production_and_never_gets_bad_input() {
        let prod = policy(PROD, Some("api.example.com"));
        let err = check(&settings(), &prod, &Ask::default()).unwrap_err();
        assert!(err.is_refusal());
        assert!(
            err.to_string().contains("[stress].allow_production"),
            "{err}"
        );
        let allowed = StressSettings {
            allow_production: vec![HostPattern::parse("api.example.com").unwrap()],
            ..settings()
        };
        assert_eq!(check(&allowed, &prod, &Ask::default()), Ok(()));
        let malformed = names(&["json-syntax"]);
        let wire = names(&["slow-body"]);
        for ask in [
            Ask {
                fuzz_ratio: Some(0.1),
                ..Ask::default()
            },
            Ask {
                malformed: &malformed,
                ..Ask::default()
            },
            Ask {
                wire: &wire,
                allow_wire_faults: true,
                ..Ask::default()
            },
        ] {
            let err = check(&allowed, &prod, &ask).unwrap_err();
            assert!(err.is_refusal());
            assert_eq!(err.to_string(), NO_BAD_INPUT_IN_PRODUCTION);
        }
    }

    #[test]
    fn costly_families_and_wire_faults_need_their_opt_ins() {
        let staging = policy(STAGING, None);
        let bomb = names(&["compression-bomb"]);
        let ask = Ask {
            malformed: &bomb,
            ..Ask::default()
        };
        let err = check(&settings(), &staging, &ask).unwrap_err();
        assert!(err.is_refusal());
        assert!(err.to_string().contains("[stress].families"), "{err}");
        let opted = StressSettings {
            families: names(&["compression-bomb"]),
            ..settings()
        };
        assert_eq!(check(&opted, &staging, &ask), Ok(()));
        let wire = names(&["slow-body"]);
        let ask = Ask {
            wire: &wire,
            ..Ask::default()
        };
        let err = check(&settings(), &staging, &ask).unwrap_err();
        assert!(err.is_refusal());
        assert!(err.to_string().contains("--allow-wire-faults"), "{err}");
        let allowed = Ask {
            allow_wire_faults: true,
            ..ask
        };
        assert_eq!(check(&settings(), &staging, &allowed), Ok(()));
    }

    #[test]
    fn wire_consent_needs_every_rule_and_carries_the_connection_cap() {
        let staging = policy(STAGING, None);
        let wire = names(&["slow-body", "cl-te-conflict"]);
        let refused = Ask {
            wire: &wire,
            ..Ask::default()
        };
        assert!(
            wire_consent(&settings(), &staging, &refused)
                .unwrap_err()
                .is_refusal()
        );
        let asked = Ask {
            allow_wire_faults: true,
            ..refused
        };
        let consent = wire_consent(&settings(), &staging, &asked)
            .unwrap()
            .unwrap();
        assert_eq!(
            consent.families(),
            [WireFamily::SlowBody, WireFamily::ClTeConflict]
        );
        assert_eq!(consent.max_connections(), 20);
        let unlisted = policy("https://api.other.com", None);
        assert!(
            wire_consent(&settings(), &unlisted, &asked)
                .unwrap_err()
                .is_refusal()
        );
        assert_eq!(
            wire_consent(&settings(), &staging, &Ask::default()),
            Ok(None)
        );
    }

    #[test]
    fn bad_ratios_and_unknown_families_are_usage_errors() {
        let staging = policy(STAGING, None);
        for ratio in [0.0, -0.1, 1.5, f64::NAN] {
            let ask = Ask {
                fuzz_ratio: Some(ratio),
                ..Ask::default()
            };
            let err = check(&settings(), &staging, &ask).unwrap_err();
            assert!(!err.is_refusal(), "{ratio}");
        }
        let one = Ask {
            fuzz_ratio: Some(1.0),
            ..Ask::default()
        };
        assert_eq!(check(&settings(), &staging, &one), Ok(()));
        let bad = names(&["teapot"]);
        let ask = Ask {
            wire: &bad,
            allow_wire_faults: true,
            ..Ask::default()
        };
        let err = check(&settings(), &staging, &ask).unwrap_err();
        assert!(!err.is_refusal());
        assert!(err.to_string().contains("unknown family 'teapot'"), "{err}");
    }
}
