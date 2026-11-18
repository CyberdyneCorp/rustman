//! Signature verifiers per path (design D4): `[[webhook.verifiers]]` in
//! `rustman.toml`, each with a `{{variable}}` secret resolved from the
//! active environment. The secret never enters a capture, the event log or
//! `Debug` output.
//!
//! ```toml
//! [[webhook.verifiers]]
//! path = "/hooks/github"     # exact, or a glob: * within a segment, ** across
//! type = "github"            # github, stripe or hmac
//! secret = "{{gh_hook}}"
//!
//! [[webhook.verifiers]]
//! path = "/hooks/shop/**"
//! type = "hmac"
//! header = "X-Signature"
//! algorithm = "sha256"       # sha1, sha256 (default) or sha512
//! encoding = "base64"        # hex (default) or base64
//! prefix = ""                # text before the encoded MAC
//! secret = "{{shop_secret}}"
//! ```

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use globset::{GlobBuilder, GlobMatcher};
use rustman_core::settings::SETTINGS_FILE;
use rustman_core::vars::{EnvironmentSource, Lookup, Resolver, SecretLookup, Segment, Template};
use rustman_core::{ConfigError, Environment, LoadError, Redactor, Secret, Workspace};
use serde::Deserialize;
use time::OffsetDateTime;

use crate::capture::HeaderEntry;
use crate::settings::WebhookSettings;
use crate::signature::{Algorithm, Encoding, HmacScheme, STRIPE_TOLERANCE, Scheme, Signature};

/// One `[[webhook.verifiers]]` table as written.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifierSpec {
    pub path: String,
    #[serde(rename = "type")]
    pub kind: String,
    /// A `{{variable}}` reference; literal keys are refused.
    pub secret: String,
    pub header: Option<String>,
    pub algorithm: Option<String>,
    pub encoding: Option<String>,
    pub prefix: Option<String>,
    /// Stripe only: the oldest accepted timestamp, in seconds.
    pub tolerance_s: Option<u64>,
}

impl VerifierSpec {
    fn scheme(&self) -> Result<Scheme, String> {
        let generic = [&self.header, &self.algorithm, &self.encoding, &self.prefix];
        let only_hmac =
            || "header, algorithm, encoding and prefix apply only to type hmac".to_owned();
        match self.kind.as_str() {
            "hmac" => self.hmac_scheme().map(Scheme::Hmac),
            _ if generic.iter().any(|f| f.is_some()) => Err(only_hmac()),
            _ if self.tolerance_s.is_some() && self.kind != "stripe" => {
                Err("tolerance_s applies only to type stripe".to_owned())
            }
            "github" => Ok(Scheme::Hmac(HmacScheme::github())),
            "stripe" => Ok(Scheme::Stripe {
                tolerance: self
                    .tolerance_s
                    .map_or(STRIPE_TOLERANCE, Duration::from_secs),
            }),
            other => Err(format!(
                "unknown type {other:?} (use github, stripe or hmac)"
            )),
        }
    }

    fn hmac_scheme(&self) -> Result<HmacScheme, String> {
        if self.tolerance_s.is_some() {
            return Err("tolerance_s applies only to type stripe".to_owned());
        }
        let header = self
            .header
            .clone()
            .filter(|h| !h.trim().is_empty())
            .ok_or("type hmac needs a header")?;
        Ok(HmacScheme {
            header,
            algorithm: self
                .algorithm
                .as_deref()
                .map_or(Ok(Algorithm::default()), Algorithm::parse)?,
            encoding: self
                .encoding
                .as_deref()
                .map_or(Ok(Encoding::default()), Encoding::parse)?,
            prefix: self.prefix.clone().unwrap_or_default(),
        })
    }
}

/// The variable name of a secret written as `{{name}}`.
fn secret_name(text: &str) -> Result<String, String> {
    let refuse = || {
        "secret must be a {{variable}} reference (keep the key in an environment secret, \
         not in rustman.toml)"
            .to_owned()
    };
    let template = Template::parse(text.trim()).map_err(|_| refuse())?;
    match template.segments() {
        [Segment::Var(name)] => Ok(name.clone()),
        _ => Err(refuse()),
    }
}

/// The value of `name` from `resolver`.
fn resolve_secret(resolver: &Resolver<'_>, name: &str) -> Result<Secret<String>, String> {
    match resolver.lookup(name) {
        Some(Lookup::Secret(secret)) => Ok(secret),
        Some(Lookup::Value(value)) => Ok(Secret::new(name, value)),
        Some(Lookup::SecretNotSet { hint }) => Err(format!("{name}: {hint}")),
        None => Err(format!("unresolved variable(s): {name}")),
    }
}

/// A verifier ready to check requests.
#[derive(Clone, Debug)]
pub struct Verifier {
    /// The path pattern as written.
    pub pattern: String,
    matcher: GlobMatcher,
    pub scheme: Scheme,
    secret: Secret<String>,
}

impl Verifier {
    /// A verifier for `pattern` with `scheme` and the key `secret`.
    pub fn new(pattern: &str, scheme: Scheme, secret: Secret<String>) -> Result<Self, String> {
        let matcher = GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|e| format!("invalid path pattern {pattern:?}: {}", e.kind()))?
            .compile_matcher();
        Ok(Self {
            pattern: pattern.to_owned(),
            matcher,
            scheme,
            secret,
        })
    }

    /// Build the verifier `spec` describes, reading its secret from
    /// `resolver`.
    pub fn resolve(spec: &VerifierSpec, resolver: &Resolver<'_>) -> Result<Self, String> {
        let scheme = spec.scheme()?;
        let name = secret_name(&spec.secret)?;
        let secret = resolve_secret(resolver, &name)?;
        Self::new(&spec.path, scheme, secret)
    }

    pub fn matches(&self, path: &str) -> bool {
        self.matcher.is_match(path)
    }

    pub fn check(&self, headers: &[HeaderEntry], body: &[u8], now: OffsetDateTime) -> Signature {
        let key = self.secret.expose().as_bytes();
        self.scheme.check(key, headers, body, now)
    }
}

/// The verifiers of a listener, first match by path wins. Cloning shares
/// them.
#[derive(Clone, Debug, Default)]
pub struct Verifiers {
    list: Arc<[Verifier]>,
}

impl Verifiers {
    pub fn new(list: Vec<Verifier>) -> Self {
        Self { list: list.into() }
    }

    /// Resolve every spec; errors name the verifier and are in
    /// `rustman.toml`.
    pub fn resolve(specs: &[VerifierSpec], resolver: &Resolver<'_>) -> Result<Self, LoadError> {
        let list = specs
            .iter()
            .enumerate()
            .map(|(i, spec)| {
                Verifier::resolve(spec, resolver).map_err(|e| {
                    let message = format!("webhook verifier {} ({}): {e}", i + 1, spec.path);
                    LoadError::new(Path::new(SETTINGS_FILE), None, message)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::new(list))
    }

    /// The verifiers `specs`, their secrets read from `environment`
    /// (through `secrets`) only: never from `[variables]` in
    /// `rustman.toml`, which is meant for Git.
    pub fn for_environment(
        specs: &[VerifierSpec],
        environment: Option<&Environment>,
        secrets: &dyn SecretLookup,
    ) -> Result<Self, LoadError> {
        if specs.is_empty() {
            return Ok(Self::default());
        }
        let mut resolver = Resolver::new();
        if let Some(environment) = environment {
            resolver.push(EnvironmentSource::new(environment, secrets));
        }
        Self::resolve(specs, &resolver)
    }

    /// The verifiers of `settings` for the workspace at `root`, as every
    /// front-end starts the listener: the environment named by `explicit`,
    /// else `env_var` (`RUSTMAN_ENV`), else the workspace default, its
    /// secrets read from the store `open_secrets` opens. Nothing is read
    /// when there are no verifiers.
    pub fn load<S: SecretLookup>(
        root: &Path,
        settings: &WebhookSettings,
        explicit: Option<&str>,
        env_var: Option<&str>,
        open_secrets: impl FnOnce() -> Result<S, ConfigError>,
    ) -> Result<Self, ConfigError> {
        if settings.verifiers.is_empty() {
            return Ok(Self::default());
        }
        let ws = Workspace::load(root)?;
        let active = ws.active_environment(explicit, env_var)?;
        let secrets = open_secrets()?;
        Self::for_environment(&settings.verifiers, active, &secrets)
            .map_err(|e| ConfigError::Invalid(vec![e]))
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Verifier> {
        self.list.iter()
    }

    /// The signature status of a request to `path`, `None` when no verifier
    /// covers that path.
    pub fn check(
        &self,
        path: &str,
        headers: &[HeaderEntry],
        body: &[u8],
        now: OffsetDateTime,
    ) -> Option<Signature> {
        self.list
            .iter()
            .find(|v| v.matches(path))
            .map(|v| v.check(headers, body, now))
    }

    /// A redactor that knows every verifier secret, for output sinks.
    pub fn redactor(&self) -> Redactor {
        let mut redactor = Redactor::new();
        for verifier in self.list.iter() {
            redactor.add(&verifier.secret);
        }
        redactor
    }
}

#[cfg(test)]
mod tests {
    use rustman_core::Environment;
    use rustman_core::vars::{EnvironmentSource, SecretLookup};

    use super::*;
    use crate::signature::tests::hex;

    const CANARY: &str = "canary-whsec-5f1e2d";

    struct Store;

    impl SecretLookup for Store {
        fn secret(&self, _env: &str, name: &str) -> Result<Option<String>, String> {
            Ok((name == "gh_hook").then(|| CANARY.to_owned()))
        }
    }

    fn environment() -> Environment {
        Environment {
            name: "local".into(),
            secrets: vec!["gh_hook".into(), "unset".into()],
            ..Environment::default()
        }
    }

    fn spec(kind: &str, secret: &str) -> VerifierSpec {
        VerifierSpec {
            path: "/hooks/**".into(),
            kind: kind.into(),
            secret: secret.into(),
            ..VerifierSpec::default()
        }
    }

    fn resolve(spec: &VerifierSpec) -> Result<Verifier, String> {
        let env = environment();
        let resolver = Resolver::new().with(EnvironmentSource::new(&env, &Store));
        Verifier::resolve(spec, &resolver)
    }

    #[test]
    fn secrets_come_from_the_environment_secret_store() {
        let verifier = resolve(&spec("github", "{{gh_hook}}")).unwrap();
        let mac = Algorithm::Sha256.hmac(CANARY.as_bytes(), &[b"{}"]);
        let headers = [HeaderEntry::new(
            "X-Hub-Signature-256",
            format!("sha256={}", hex(&mac)),
        )];
        let now = OffsetDateTime::UNIX_EPOCH;
        assert_eq!(verifier.check(&headers, b"{}", now), Signature::Valid);
        assert!(!format!("{verifier:?}").contains(CANARY));
    }

    /// A workspace whose verifier reads `{{key}}`, with `key` in
    /// `[variables]` and, when `env_yaml` is given, `environments/local.yaml`.
    fn workspace(env_yaml: Option<&str>) -> (tempfile::TempDir, WebhookSettings) {
        let dir = tempfile::tempdir().unwrap();
        let toml = "[workspace]\nname = \"w\"\nversion = 1\n\n[variables]\nkey = \"from-toml\"\n\n[[webhook.verifiers]]\npath = \"/h\"\ntype = \"github\"\nsecret = \"{{key}}\"\n";
        std::fs::write(dir.path().join("rustman.toml"), toml).unwrap();
        if let Some(yaml) = env_yaml {
            std::fs::create_dir(dir.path().join("environments")).unwrap();
            std::fs::write(dir.path().join("environments/local.yaml"), yaml).unwrap();
        }
        let settings = WebhookSettings::load(dir.path()).unwrap();
        (dir, settings)
    }

    #[test]
    fn workspace_variables_never_provide_a_secret() {
        let (dir, settings) = workspace(None);
        let err = Verifiers::load(dir.path(), &settings, None, None, || Ok(Store))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unresolved variable(s): key"), "{err}");
    }

    #[test]
    fn load_reads_the_named_environment_and_opens_secrets_only_when_needed() {
        let (dir, settings) = workspace(Some("variables:\n  key: from-env\n"));
        let loaded = Verifiers::load(dir.path(), &settings, None, Some("local"), || Ok(Store));
        assert_eq!(
            loaded.unwrap().redactor().text("from-env"),
            "«redacted:key»"
        );
        let unknown = Verifiers::load(dir.path(), &settings, Some("prod"), None, || Ok(Store));
        assert!(matches!(
            unknown,
            Err(ConfigError::EnvironmentNotFound { .. })
        ));
        let none = WebhookSettings::default();
        let opened = Verifiers::load(dir.path(), &none, None, None, || -> Result<Store, _> {
            panic!("no verifiers: the secret store is not opened")
        });
        assert!(opened.unwrap().is_empty());
    }

    #[test]
    fn literal_unset_and_unknown_secrets_are_refused() {
        let literal = resolve(&spec("github", "s3cr3t")).unwrap_err();
        assert!(
            literal.starts_with("secret must be a {{variable}} reference"),
            "{literal}"
        );
        assert!(resolve(&spec("github", "x{{gh_hook}}")).is_err());
        let unset = resolve(&spec("github", "{{unset}}")).unwrap_err();
        assert!(
            unset.contains("rustman secret set unset --env local"),
            "{unset}"
        );
        let unknown = resolve(&spec("github", "{{nope}}")).unwrap_err();
        assert_eq!(unknown, "unresolved variable(s): nope");
    }

    #[test]
    fn spec_validation() {
        let err = |s: VerifierSpec| resolve(&s).unwrap_err();
        assert!(err(spec("gitlab", "{{gh_hook}}")).starts_with("unknown type \"gitlab\""));
        assert_eq!(err(spec("hmac", "{{gh_hook}}")), "type hmac needs a header");
        let mut github = spec("github", "{{gh_hook}}");
        github.header = Some("X".into());
        assert!(err(github).contains("apply only to type hmac"));
        let mut tolerance = spec("github", "{{gh_hook}}");
        tolerance.tolerance_s = Some(5);
        assert_eq!(err(tolerance), "tolerance_s applies only to type stripe");
        let mut hmac = spec("hmac", "{{gh_hook}}");
        hmac.header = Some("X-Signature".into());
        hmac.algorithm = Some("md5".into());
        assert!(err(hmac).starts_with("unknown algorithm"));
        let mut stripe = spec("stripe", "{{gh_hook}}");
        stripe.tolerance_s = Some(60);
        let scheme = resolve(&stripe).unwrap().scheme;
        assert_eq!(
            scheme,
            Scheme::Stripe {
                tolerance: Duration::from_secs(60)
            }
        );
    }

    #[test]
    fn first_matching_path_wins_and_unmatched_paths_are_unchecked() {
        let secret = || Secret::new("k", "key".to_owned());
        let github = Scheme::Hmac(HmacScheme::github());
        let verifiers = Verifiers::new(vec![
            Verifier::new("/hooks/github", github.clone(), secret()).unwrap(),
            Verifier::new(
                "/hooks/*",
                Scheme::Stripe {
                    tolerance: STRIPE_TOLERANCE,
                },
                secret(),
            )
            .unwrap(),
        ]);
        let now = OffsetDateTime::UNIX_EPOCH;
        assert_eq!(
            verifiers.check("/hooks/github", &[], b"", now),
            Some(Signature::Missing)
        );
        assert_eq!(
            verifiers.check("/hooks/stripe", &[], b"", now),
            Some(Signature::Missing)
        );
        assert_eq!(verifiers.check("/hooks/a/b", &[], b"", now), None);
        assert_eq!(verifiers.check("/other", &[], b"", now), None);
        assert!(Verifier::new("[", github, secret()).is_err());
    }

    #[test]
    fn the_redactor_knows_every_secret() {
        let env = environment();
        let resolver = Resolver::new().with(EnvironmentSource::new(&env, &Store));
        let verifiers = Verifiers::resolve(&[spec("github", "{{gh_hook}}")], &resolver).unwrap();
        let line = format!("sent {CANARY} back");
        assert_eq!(
            verifiers.redactor().text(&line),
            "sent «redacted:gh_hook» back"
        );
    }

    #[test]
    fn resolve_errors_name_the_verifier() {
        let resolver = Resolver::new();
        let err = Verifiers::resolve(&[spec("github", "{{gh}}")], &resolver).unwrap_err();
        assert!(
            err.to_string()
                .contains("webhook verifier 1 (/hooks/**): unresolved variable(s): gh"),
            "{err}"
        );
    }
}
