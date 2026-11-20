use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_lite::future::block_on;

use super::*;
use crate::environment::Environment;
use crate::model::{VarValue, Variables};
use crate::vars::{EnvironmentSource, NoSecrets, Resolved};

/// A provider answering from a map, counting fetches and the most
/// fetches in flight at once.
#[derive(Default)]
struct Spy {
    values: HashMap<String, String>,
    delay: Option<Duration>,
    fetches: AtomicUsize,
    in_flight: AtomicUsize,
    peak: AtomicUsize,
}

impl Spy {
    fn new(values: &[(&str, &str)], delay: Option<Duration>) -> Arc<Self> {
        Arc::new(Self {
            values: values
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
            delay,
            ..Self::default()
        })
    }

    fn with(values: &[(&str, &str)]) -> Arc<Self> {
        Self::new(values, None)
    }

    fn fetches(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }
}

impl SecretProvider for Spy {
    fn fetch<'a>(&'a self, path: &'a str) -> FetchFuture<'a> {
        Box::pin(async move {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            if let Some(delay) = self.delay {
                futures_timer::Delay::new(delay).await;
            }
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            let value = self.values.get(path).cloned();
            value
                .map(|v| Secret::new(path, v))
                .ok_or_else(|| ProviderError::NotFound(format!("spy: {path} not found")))
        })
    }
}

fn info(name: &str, timeout: Duration) -> ProviderInfo {
    ProviderInfo {
        name: name.into(),
        kind: "spy".into(),
        origin: Origin::Workspace,
        auth: None,
        timeout,
        ttl: DEFAULT_TTL,
    }
}

fn providers(name: &str, provider: Arc<dyn SecretProvider>) -> SecretProviders {
    let mut providers = SecretProviders::new();
    providers.insert(info(name, DEFAULT_TIMEOUT), provider);
    providers
}

fn resolve(
    resolver: &mut Resolver<'_>,
    providers: Option<&SecretProviders>,
    text: &str,
) -> Result<Resolved, ResolveError> {
    block_on(resolve_with_secrets(resolver, providers, |r| {
        r.resolve(text)
    }))
}

#[test]
fn reference_in_a_header_carries_the_field() {
    let spy = Spy::with(&[(
        "kv/app/stripe",
        r#"{"api_key":"sk_live_canary","other":"x"}"#,
    )]);
    let providers = providers("vault", spy.clone());
    let mut resolver = Resolver::new();
    let header = "Bearer {{secret:vault/kv/app/stripe#api_key}}";
    let resolved = resolve(&mut resolver, Some(&providers), header).unwrap();
    assert_eq!(resolved.text, "Bearer sk_live_canary");
    assert_eq!(resolved.secret_spans[0].name, "vault/kv/app/stripe#api_key");
    assert_eq!(spy.fetches(), 1);
    // The used secret is labelled with the reference, for the redactor.
    let mut i = resolver.interpolator();
    let r = i.text(header);
    let (_, used) = i.finish(r).unwrap();
    let redactor = crate::Redactor::new().with(&used[0]);
    assert_eq!(
        redactor.text("echo sk_live_canary"),
        "echo «redacted:vault/kv/app/stripe#api_key»"
    );
    assert!(!format!("{resolved:?}").contains("sk_live_canary"));
}

#[test]
fn templates_without_references_never_fetch() {
    let spy = Spy::with(&[]);
    let providers = providers("vault", spy.clone());
    let mut resolver = Resolver::new();
    let resolved = resolve(&mut resolver, Some(&providers), "plain {{$uuid}}").unwrap();
    assert!(resolved.text.starts_with("plain "));
    assert_eq!(spy.fetches(), 0);
}

#[test]
fn resolving_outside_a_send_names_the_reference() {
    let err = Resolver::new()
        .resolve("{{secret:vault/kv/app#pw}}")
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "secret reference(s) vault/kv/app#pw are resolved only when a request is sent"
    );
    let kept = Resolver::new()
        .keeping_secret_refs()
        .resolve("Bearer {{secret:vault/kv/app#pw}}")
        .unwrap();
    assert_eq!(kept.text, "Bearer {{secret:vault/kv/app#pw}}");
}

#[test]
fn provider_backed_variables_are_labelled_by_name() {
    let reference = SecretRef::parse("op/Engineering/Stripe/token").unwrap();
    let env = Environment {
        name: "prod".into(),
        variables: Variables::new(vec![("api_token".into(), VarValue::Secret(reference))]),
        ..Environment::default()
    };
    let spy = Spy::with(&[("Engineering/Stripe/token", "tok-canary")]);
    let providers = providers("op", spy.clone());
    let mut resolver = Resolver::new().with(EnvironmentSource::new(&env, &NoSecrets));
    let resolved = resolve(&mut resolver, Some(&providers), "{{api_token}}").unwrap();
    assert_eq!(resolved.text, "tok-canary");
    assert_eq!(resolved.secret_spans[0].name, "api_token");
    assert_eq!(spy.fetches(), 1);
}

#[test]
fn unknown_providers_list_the_configured_ones() {
    let providers = providers("vault", Spy::with(&[]));
    let mut resolver = Resolver::new();
    let err = resolve(&mut resolver, Some(&providers), "{{secret:lastpass/x}}").unwrap_err();
    assert_eq!(
        err.to_string(),
        "unresolved secret lastpass/x: unknown secret provider 'lastpass' (configured: vault)"
    );
    let err = resolve(&mut Resolver::new(), None, "{{secret:lastpass/x}}").unwrap_err();
    assert!(err.to_string().contains("(configured: none)"), "{err}");
}

#[test]
fn missing_field_is_unresolved_without_the_value() {
    let spy = Spy::with(&[("kv/app/db", r#"{"password":"pw-canary"}"#)]);
    let providers = providers("vault", spy);
    let err = resolve(
        &mut Resolver::new(),
        Some(&providers),
        "{{secret:vault/kv/app/db#user}}",
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "unresolved secret vault/kv/app/db#user: field 'user' not found"
    );
    let ResolveError::Secret(unresolved) = err else {
        panic!("not a secret error");
    };
    assert_eq!(unresolved.exit_code(), 3);
}

#[test]
fn slow_providers_time_out() {
    let spy = Spy::new(&[("kv/app", "v")], Some(Duration::from_secs(30)));
    let mut providers = SecretProviders::new();
    providers.insert(info("vault", Duration::from_millis(50)), spy);
    let err = resolve(
        &mut Resolver::new(),
        Some(&providers),
        "{{secret:vault/kv/app}}",
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "unresolved secret vault/kv/app: vault: timed out after 50 ms"
    );
}

#[test]
fn missing_variables_are_reported_before_any_fetch() {
    let spy = Spy::with(&[("kv/app", "v")]);
    let providers = providers("vault", spy.clone());
    let err = resolve(
        &mut Resolver::new(),
        Some(&providers),
        "{{nope}} {{secret:vault/kv/app}}",
    )
    .unwrap_err();
    assert!(matches!(err, ResolveError::Unresolved(_)), "{err}");
    assert_eq!(spy.fetches(), 0);
}

#[test]
fn a_run_reuses_a_fetched_secret() {
    let spy = Spy::with(&[("kv/app", r#"{"pw":"p"}"#)]);
    let providers = providers("vault", spy.clone());
    for _ in 0..50 {
        let mut resolver = Resolver::new();
        let r = resolve(
            &mut resolver,
            Some(&providers),
            "{{secret:vault/kv/app#pw}}",
        )
        .unwrap();
        assert_eq!(r.text, "p");
    }
    assert_eq!(spy.fetches(), 1);
    providers.refresh();
    resolve(
        &mut Resolver::new(),
        Some(&providers),
        "{{secret:vault/kv/app#pw}}",
    )
    .unwrap();
    assert_eq!(spy.fetches(), 2);
}

#[test]
fn fetches_of_one_request_are_capped() {
    let values: Vec<(String, String)> = (0..10)
        .map(|i| (format!("s{i}"), format!("v{i}")))
        .collect();
    let pairs: Vec<(&str, &str)> = values
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let spy = Spy::new(&pairs, Some(Duration::from_millis(20)));
    let providers = providers("p", spy.clone());
    let text: String = (0..10).map(|i| format!("{{{{secret:p/s{i}}}}}")).collect();
    let r = resolve(&mut Resolver::new(), Some(&providers), &text).unwrap();
    assert_eq!(r.text, (0..10).map(|i| format!("v{i}")).collect::<String>());
    assert_eq!(spy.fetches(), 10);
    assert!(spy.peak.load(Ordering::SeqCst) <= MAX_CONCURRENT_FETCHES);
}

/// Logs fetches, so a test can show that a code path made none.
struct Recorder(Mutex<Vec<String>>);

impl SecretProvider for Recorder {
    fn fetch<'a>(&'a self, path: &'a str) -> FetchFuture<'a> {
        self.0.lock().unwrap().push(path.to_owned());
        Box::pin(async move { Ok(Secret::new(path, "v".to_owned())) })
    }
}

/// "Opening the TUI doesn't fetch" and "Saving keeps the reference": the
/// work behind opening and browsing (loading, listing, rendering request
/// definitions) and saving requests and environments contacts nothing.
#[test]
fn loading_listing_rendering_and_saving_make_no_fetch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join(crate::settings::SETTINGS_FILE),
        "[workspace]\nname = \"w\"\nversion = 1\n\n[secret_providers.vault]\ntype = \"vault\"\naddress = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();
    std::fs::create_dir(root.join("environments")).unwrap();
    std::fs::write(
        root.join("environments/prod.yaml"),
        "variables:\n  api_token: {secret: \"vault/kv/app#token\"}\n",
    )
    .unwrap();
    for i in 0..40 {
        std::fs::write(
            root.join(format!("r{i}.rman.yaml")),
            format!(
                "version: 1\nkind: http\nname: r{i}\nmethod: GET\nurl: https://example.com/{i}\nheaders:\n  - name: Authorization\n    value: Bearer {{{{secret:vault/kv/app#pw}}}} {{{{api_token}}}}\n"
            ),
        )
        .unwrap();
    }
    let recorder = Arc::new(Recorder(Mutex::new(Vec::new())));
    let providers = providers("vault", recorder.clone());

    let ws = crate::Workspace::load(root).unwrap();
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let requests = ws.requests();
    assert_eq!(requests.len(), 40);
    for request in &requests {
        // Rendering goes through the same entry point as a send, with the
        // providers at hand: placeholders never need their values.
        let mut resolver = Resolver::for_request(&ws.settings, request, env, &NoSecrets);
        let render = |r: &Resolver<'_>| crate::vars::resolve_placeholders(request, r);
        let (shown, _) = block_on(resolve_with_secrets(
            &mut resolver,
            Some(&providers),
            render,
        ))
        .unwrap();
        assert_eq!(
            shown.headers[0].value.text,
            "Bearer {{secret:vault/kv/app#pw}} «redacted:api_token»"
        );
        let saved = crate::format::write::request_to_yaml(request.request());
        assert!(saved.contains("{{secret:vault/kv/app#pw}}"), "{saved}");
    }
    let env_text = crate::format::write::environment_to_yaml(env.unwrap());
    assert!(
        env_text.contains("secret: vault/kv/app#token"),
        "{env_text}"
    );
    let reloaded = crate::environment::parse(
        std::path::Path::new("environments/prod.yaml"),
        "prod",
        &env_text,
    )
    .unwrap();
    assert_eq!(&reloaded, env.unwrap());
    assert!(recorder.0.lock().unwrap().is_empty());

    // The spy is live: a send of the same request does fetch.
    let request = &requests[0];
    let mut resolver = Resolver::for_request(&ws.settings, request, env, &NoSecrets);
    let send = |r: &Resolver<'_>| crate::vars::resolve_request(request, r);
    // ("v" has no field `pw`: only the fetch matters here.)
    let _ = block_on(resolve_with_secrets(&mut resolver, Some(&providers), send));
    let fetched = recorder.0.lock().unwrap().clone();
    assert_eq!(fetched, ["kv/app"]);
}

#[test]
fn malformed_references_fail_workspace_loading() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join(crate::settings::SETTINGS_FILE),
        "[workspace]\nname = \"w\"\nversion = 1\n",
    )
    .unwrap();
    std::fs::write(
        root.join("bad.rman.yaml"),
        "version: 1\nkind: http\nname: bad\nmethod: GET\nurl: https://example.com/{{secret:vault}}\n",
    )
    .unwrap();
    let err = crate::Workspace::load(root).unwrap_err();
    assert_eq!(
        err.to_string(),
        "bad.rman.yaml:5: invalid secret reference 'secret:vault' (expected secret:<provider>/<path>)"
    );
}

/// A provider whose health check fails or never answers.
struct Unhealthy {
    hang: bool,
}

impl SecretProvider for Unhealthy {
    fn fetch<'a>(&'a self, _: &'a str) -> FetchFuture<'a> {
        Box::pin(async { Err(ProviderError::Unavailable("x".into())) })
    }

    fn check<'a>(&'a self) -> LoginFuture<'a> {
        Box::pin(async move {
            if self.hang {
                futures_timer::Delay::new(Duration::from_secs(60)).await;
            }
            Err(ProviderError::Auth("vault: authentication failed".into()))
        })
    }
}

#[test]
fn health_checks_report_auth_failures_timeouts_and_unknown_names() {
    let mut set = SecretProviders::new();
    set.insert(info("ok", DEFAULT_TIMEOUT), Spy::with(&[]));
    set.insert(
        info("vault", DEFAULT_TIMEOUT),
        Arc::new(Unhealthy { hang: false }),
    );
    set.insert(
        info("slow", Duration::from_millis(20)),
        Arc::new(Unhealthy { hang: true }),
    );
    assert_eq!(block_on(set.check("ok")), Ok(()));
    let err = block_on(set.check("vault")).unwrap_err();
    assert_eq!(err.to_string(), "vault: authentication failed");
    assert_eq!(err.exit_code(), EXIT_REQUEST);
    let err = block_on(set.check("slow")).unwrap_err();
    assert_eq!(err.to_string(), "slow: timed out after 20 ms");
    let err = block_on(set.check("nope")).unwrap_err();
    assert!(
        err.to_string()
            .starts_with("unknown secret provider 'nope'")
    );
}
