//! The safety policy's gating rules, one table row per rule (task 2.1).

use rustman_core::safety::{
    Blocked, EnvironmentInfo, HostPattern, Origin, Permission, PolicyError, PolicyOptions,
    SafetyPolicy, SafetySettings,
};

const TARGET: &str = "https://api.example.com/v1";

fn settings(allowed: &[&str], production: &[&str]) -> SafetySettings {
    SafetySettings {
        allowed_hosts: allowed.iter().map(|o| Origin::parse(o).unwrap()).collect(),
        production_hosts: production
            .iter()
            .map(|h| HostPattern::parse(h).unwrap())
            .collect(),
        ..SafetySettings::default()
    }
}

fn env(name: &str) -> EnvironmentInfo<'_> {
    EnvironmentInfo {
        name: Some(name),
        marked_production: false,
    }
}

fn staging_policy(permission: Permission) -> SafetyPolicy {
    let options = PolicyOptions {
        target: TARGET,
        permission,
        environment: env("staging"),
        ..PolicyOptions::default()
    };
    SafetyPolicy::new(&SafetySettings::default(), options).unwrap()
}

use Blocked::*;
use Permission::{Destructive, Mutations, ReadOnly};

#[test]
fn method_gating() {
    let url = "https://api.example.com/v1/users";
    let rows: &[(Permission, &str, Result<(), Blocked>)] = &[
        (ReadOnly, "GET", Ok(())),
        (ReadOnly, "HEAD", Ok(())),
        (ReadOnly, "OPTIONS", Ok(())),
        (ReadOnly, "POST", Err(MutationsNotAllowed)),
        (ReadOnly, "PUT", Err(MutationsNotAllowed)),
        (ReadOnly, "PATCH", Err(MutationsNotAllowed)),
        (ReadOnly, "DELETE", Err(DestructiveNotAllowed)),
        (Mutations, "GET", Ok(())),
        (Mutations, "POST", Ok(())),
        (Mutations, "PUT", Ok(())),
        (Mutations, "PATCH", Ok(())),
        (Mutations, "DELETE", Err(DestructiveNotAllowed)),
        (Destructive, "POST", Ok(())),
        (Destructive, "DELETE", Ok(())),
        // Methods we can't classify need --allow-destructive.
        (ReadOnly, "TRACE", Err(DestructiveNotAllowed)),
        (Mutations, "PURGE", Err(DestructiveNotAllowed)),
        (Mutations, "get", Err(DestructiveNotAllowed)),
        (Destructive, "PURGE", Ok(())),
    ];
    for (permission, method, expected) in rows {
        let policy = staging_policy(*permission);
        assert_eq!(
            &policy.check(method, url),
            expected,
            "{permission:?} {method}"
        );
        assert_eq!(
            &policy.check_method(method),
            expected,
            "{permission:?} {method}"
        );
    }
}

#[test]
fn allow_destructive_implies_allow_mutations() {
    assert_eq!(Permission::from_flags(false, false), ReadOnly);
    assert_eq!(Permission::from_flags(true, false), Mutations);
    assert_eq!(Permission::from_flags(false, true), Destructive);
    assert_eq!(Permission::from_flags(true, true), Destructive);
}

#[test]
fn origin_allowlist() {
    let extra = [Origin::parse("http://127.0.0.1:9000").unwrap()];
    let options = PolicyOptions {
        target: TARGET,
        allow_hosts: &extra,
        ..PolicyOptions::default()
    };
    let settings = settings(&["https://auth.example.com"], &[]);
    let policy = SafetyPolicy::new(&settings, options).unwrap();
    let rows: &[(&str, Result<(), Blocked>)] = &[
        // The target's own origin, any path.
        ("https://api.example.com/v1/users/1", Ok(())),
        ("https://API.EXAMPLE.COM:443/other", Ok(())),
        // [safety].allowed_hosts and --allow-host.
        ("https://auth.example.com/token", Ok(())),
        ("http://127.0.0.1:9000/x", Ok(())),
        // Any other origin: host, scheme or port differs.
        ("https://other.example.net/", Err(HostNotAllowed)),
        ("http://api.example.com/v1", Err(HostNotAllowed)),
        ("https://api.example.com:8443/v1", Err(HostNotAllowed)),
        ("https://api.example.com.evil.net/", Err(HostNotAllowed)),
        ("https://api.example.com@evil.net/", Err(HostNotAllowed)),
        ("http://127.0.0.1:9001/x", Err(HostNotAllowed)),
        // URLs with no http(s) origin, such as ones built from response data.
        ("ftp://api.example.com/", Err(HostNotAllowed)),
        ("/v1/users", Err(HostNotAllowed)),
        ("", Err(HostNotAllowed)),
    ];
    for (url, expected) in rows {
        assert_eq!(&policy.check("GET", url), expected, "{url}");
        assert_eq!(&policy.check_origin(url), expected, "{url}");
    }
    // The host rule comes before the method rule.
    assert_eq!(
        policy.check("DELETE", "https://other.example.net/"),
        Err(HostNotAllowed)
    );
    assert_eq!(
        policy
            .allowed_origins()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        [
            "https://api.example.com:443",
            "https://auth.example.com:443",
            "http://127.0.0.1:9000",
        ]
    );
}

#[test]
fn production_detection_and_guard() {
    type Row<'a> = (
        &'a str,
        EnvironmentInfo<'a>,
        Option<&'a str>,
        Permission,
        Result<(), &'a str>,
    );
    let marked = EnvironmentInfo {
        name: Some("live"),
        marked_production: true,
    };
    let refused = "refusing to send requests to production host 'api.example.com'";
    let destructive = "refusing --allow-destructive against production host 'api.example.com'";
    let rows: &[Row<'_>] = &[
        // Not production: everything the flags allow.
        (TARGET, env("staging"), None, Destructive, Ok(())),
        (
            TARGET,
            EnvironmentInfo::default(),
            None,
            Destructive,
            Ok(()),
        ),
        // Each detection rule refuses without --production.
        (TARGET, env("prod"), None, ReadOnly, Err(refused)),
        (TARGET, env("production"), None, ReadOnly, Err(refused)),
        (TARGET, env("PROD"), None, ReadOnly, Err(refused)),
        (TARGET, marked, None, ReadOnly, Err(refused)),
        (
            "https://api.prod.example.org/",
            env("staging"),
            None,
            ReadOnly,
            Err("refusing to send requests to production host 'api.prod.example.org'"),
        ),
        // --production repeating the exact target host allows non-destructive requests.
        (
            TARGET,
            env("production"),
            Some("api.example.com"),
            ReadOnly,
            Ok(()),
        ),
        (
            TARGET,
            env("production"),
            Some("API.example.com"),
            Mutations,
            Ok(()),
        ),
        (TARGET, marked, Some("api.example.com"), ReadOnly, Ok(())),
        // ...and never destructive ones.
        (
            TARGET,
            env("production"),
            Some("api.example.com"),
            Destructive,
            Err(destructive),
        ),
        (TARGET, env("production"), None, Destructive, Err(refused)),
        // --production naming any other host is refused.
        (
            TARGET,
            env("production"),
            Some("example.com"),
            ReadOnly,
            Err("--production 'example.com' does not match the target host 'api.example.com'"),
        ),
        (
            TARGET,
            env("production"),
            Some("https://api.example.com"),
            ReadOnly,
            Err("--production 'https://api.example.com' does not match"),
        ),
        (
            TARGET,
            env("staging"),
            Some("other.example.net"),
            ReadOnly,
            Err("--production 'other.example.net' does not match"),
        ),
        // A bad target is input error, not a refusal.
        (
            "not a url",
            env("staging"),
            None,
            ReadOnly,
            Err("invalid target: invalid URL 'not a url'"),
        ),
    ];
    let settings = settings(&[], &["*.prod.example.org"]);
    for (target, environment, production, permission, expected) in rows {
        let options = PolicyOptions {
            target,
            permission: *permission,
            production: *production,
            environment: *environment,
            ..PolicyOptions::default()
        };
        let result = SafetyPolicy::new(&settings, options).map(|_| ());
        let row = format!("{target} {environment:?} {production:?} {permission:?}");
        match (result, expected) {
            (Ok(()), Ok(())) => {}
            (Err(err), Err(start)) => {
                assert!(err.to_string().starts_with(start), "{row}: {err}");
                let invalid = start.starts_with("invalid target");
                assert_eq!(err.is_refusal(), !invalid, "{row}");
            }
            (result, _) => panic!("{row}: unexpected {result:?}"),
        }
    }
}

#[test]
fn refusals_explain_the_guard() {
    let options = PolicyOptions {
        target: TARGET,
        environment: env("production"),
        ..PolicyOptions::default()
    };
    let err = SafetyPolicy::new(&SafetySettings::default(), options).unwrap_err();
    assert_eq!(
        err.to_string(),
        "refusing to send requests to production host 'api.example.com' (environment 'production' is a production name); repeat the host with --production api.example.com to allow non-destructive requests"
    );
    let options = PolicyOptions {
        production: Some("api.example.com"),
        permission: Destructive,
        ..options
    };
    let err = SafetyPolicy::new(&SafetySettings::default(), options).unwrap_err();
    assert!(matches!(err, PolicyError::DestructiveOnProduction { .. }));
    assert_eq!(
        err.to_string(),
        "refusing --allow-destructive against production host 'api.example.com' (environment 'production' is a production name): destructive requests are never sent to production"
    );
}

#[test]
fn confirmed_production_is_read_and_mutate_only() {
    let options = PolicyOptions {
        target: TARGET,
        permission: Mutations,
        production: Some("api.example.com"),
        environment: env("prod"),
        ..PolicyOptions::default()
    };
    let policy = SafetyPolicy::new(&SafetySettings::default(), options).unwrap();
    assert!(policy.production().is_some());
    assert_eq!(policy.check("GET", TARGET), Ok(()));
    assert_eq!(policy.check("POST", TARGET), Ok(()));
    assert_eq!(policy.check("DELETE", TARGET), Err(DestructiveNotAllowed));
    assert!(staging_policy(ReadOnly).production().is_none());
}

#[test]
fn allowed_production_hosts_need_confirmation() {
    let settings = settings(
        &["https://api.example.com", "https://billing.example.com"],
        &["api.example.com", "billing.example.com"],
    );
    // A non-production target may not reach an allowed production host.
    let options = PolicyOptions {
        target: "http://localhost:8080/",
        permission: Destructive,
        environment: env("dev"),
        ..PolicyOptions::default()
    };
    let policy = SafetyPolicy::new(&settings, options).unwrap();
    assert_eq!(policy.check("GET", "http://localhost:8080/x"), Ok(()));
    assert_eq!(
        policy.check("GET", "https://api.example.com/"),
        Err(ProductionHostNotConfirmed)
    );
    // A confirmed production target reaches its own host, not other production hosts.
    let options = PolicyOptions {
        target: TARGET,
        production: Some("api.example.com"),
        environment: env("staging"),
        ..PolicyOptions::default()
    };
    let policy = SafetyPolicy::new(&settings, options).unwrap();
    assert_eq!(policy.check("GET", TARGET), Ok(()));
    assert_eq!(
        policy.check("GET", "https://billing.example.com/"),
        Err(ProductionHostNotConfirmed)
    );
}

#[test]
fn blocked_reasons_use_the_report_wording() {
    let rows = [
        (MutationsNotAllowed, "blocked: mutations not allowed"),
        (DestructiveNotAllowed, "blocked: destructive not allowed"),
        (HostNotAllowed, "blocked: host not allowed"),
        (
            ProductionHostNotConfirmed,
            "blocked: production host not confirmed",
        ),
    ];
    for (blocked, text) in rows {
        assert_eq!(blocked.to_string(), text);
        assert_eq!(format!("blocked: {}", blocked.reason()), text);
    }
}

#[test]
fn ipv6_targets_confirm_with_or_without_brackets() {
    for given in ["[::1]", "::1"] {
        let options = PolicyOptions {
            target: "http://[::1]:8080/",
            production: Some(given),
            environment: env("prod"),
            ..PolicyOptions::default()
        };
        assert!(
            SafetyPolicy::new(&SafetySettings::default(), options).is_ok(),
            "{given}"
        );
    }
}

#[test]
fn host_spellings_do_not_escape_production_hosts() {
    let settings = settings(&[], &["api.example.com", "bücher.example", "[::1]"]);
    let targets = [
        "https://api.example.com/",
        "https://api.example.com./",
        "https://API.Example.COM./",
        "https://bücher.example/",
        "https://xn--bcher-kva.example/",
        "http://[::1]:8080/",
    ];
    for target in targets {
        let options = PolicyOptions {
            target,
            permission: Destructive,
            environment: env("staging"),
            ..PolicyOptions::default()
        };
        let err = SafetyPolicy::new(&settings, options).unwrap_err();
        assert!(
            matches!(err, PolicyError::ProductionNotConfirmed { .. }),
            "{target}: {err}"
        );
    }
}

#[test]
fn production_confirmation_accepts_any_spelling_of_the_target_host() {
    let settings = settings(&[], &["api.example.com", "bücher.example"]);
    let rows = [
        ("https://api.example.com./", "api.example.com"),
        ("https://api.example.com/", "api.example.com."),
        ("https://xn--bcher-kva.example/", "bücher.example"),
        ("https://bücher.example/", "xn--bcher-kva.example"),
    ];
    for (target, given) in rows {
        let options = PolicyOptions {
            target,
            production: Some(given),
            environment: env("staging"),
            ..PolicyOptions::default()
        };
        let policy = SafetyPolicy::new(&settings, options).unwrap();
        assert_eq!(policy.check("GET", target), Ok(()), "{target} {given}");
        assert_eq!(
            policy.check("DELETE", target),
            Err(DestructiveNotAllowed),
            "{target}"
        );
    }
    let options = PolicyOptions {
        target: "https://api.example.com/",
        production: Some("*.example.com"),
        environment: env("prod"),
        ..PolicyOptions::default()
    };
    assert!(matches!(
        SafetyPolicy::new(&settings, options),
        Err(PolicyError::ProductionHostMismatch { .. })
    ));
}

#[test]
fn a_trailing_dot_hop_to_an_unconfirmed_production_host_is_blocked() {
    let settings = settings(&["https://billing.example.com"], &["billing.example.com"]);
    let options = PolicyOptions {
        target: "http://localhost:8080/",
        environment: env("dev"),
        ..PolicyOptions::default()
    };
    let policy = SafetyPolicy::new(&settings, options).unwrap();
    assert_eq!(
        policy.check("GET", "https://billing.example.com./"),
        Err(ProductionHostNotConfirmed)
    );
    assert_eq!(policy.check("GET", "http://localhost.:8080/x"), Ok(()));
}
