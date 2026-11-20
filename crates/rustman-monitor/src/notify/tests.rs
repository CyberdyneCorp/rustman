use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use rustman_core::model::{VarValue, Variables};
use rustman_core::settings::Settings;
use rustman_test_support::{GraphqlHttpServer, RecordedRequest, Reply, TestServer};

use super::*;
use crate::alerts::WebhookTemplate;

const TOKEN: &str = "canary-T0ken-value";
const HOOK_CANARY: &str = "canary-H00k-path";

/// Secrets of the `prod` environment.
struct Secrets(HashMap<&'static str, String>);

impl SecretLookup for Secrets {
    fn secret(&self, _env: &str, name: &str) -> Result<Option<String>, String> {
        Ok(self.0.get(name).cloned())
    }
}

/// Records what it shows, or fails every time.
#[derive(Default)]
struct FakeDesktop {
    shown: Mutex<Vec<(String, String)>>,
    fail: bool,
}

impl Desktop for FakeDesktop {
    fn show(&self, title: &str, text: &str) -> Result<(), String> {
        if self.fail {
            return Err("no notification server".to_owned());
        }
        self.shown
            .lock()
            .unwrap()
            .push((title.to_owned(), text.to_owned()));
        Ok(())
    }
}

fn environment(hook: &str) -> (Environment, Secrets) {
    let env = Environment {
        name: "prod".into(),
        variables: Variables::new(vec![(
            "plain_hook".into(),
            VarValue::String(hook.to_owned()),
        )]),
        secrets: vec!["slack_hook".into(), "token".into()],
        ..Environment::default()
    };
    let secrets = Secrets(HashMap::from([
        ("slack_hook", hook.to_owned()),
        ("token", TOKEN.to_owned()),
    ]));
    (env, secrets)
}

fn settings() -> Settings {
    Settings::parse(
        Path::new("rustman.toml"),
        "[workspace]\nname = \"t\"\nversion = 1\n",
    )
    .unwrap()
    .0
}

/// A firing alert whose failure message holds the token in clear (as a
/// message the runner missed would).
fn firing() -> Alert {
    Alert {
        monitor: "api-health".into(),
        environment: "prod".into(),
        kind: AlertKind::Firing,
        rule: Some("consecutive_failures: 3".into()),
        at: "2026-03-01T10:00:00Z".parse().unwrap(),
        failure: Some(format!("health/status: header x-token was {TOKEN}")),
        duration: None,
    }
}

fn webhook(url: &str, template: WebhookTemplate) -> Channel {
    Channel::Webhook {
        url: url.into(),
        template,
    }
}

struct Fixture {
    root: tempfile::TempDir,
    settings: Settings,
    env: Environment,
    secrets: Secrets,
    desktop: FakeDesktop,
    engine: HttpEngine,
}

impl Fixture {
    fn new(hook: &str) -> Self {
        let (env, secrets) = environment(hook);
        Self {
            root: tempfile::tempdir().unwrap(),
            settings: settings(),
            env,
            secrets,
            desktop: FakeDesktop::default(),
            engine: HttpEngine::new(),
        }
    }

    fn notifier(&self, timeout: Duration) -> Notifier<'_> {
        Notifier {
            root: self.root.path(),
            settings: &self.settings,
            environment: &self.env,
            secrets: &self.secrets,
            desktop: &self.desktop,
            engine: &self.engine,
            timeout,
        }
    }

    async fn send(&self, channels: &[Channel], alert: &Alert) -> Vec<Delivery> {
        self.notifier(TIMEOUT).send(channels, alert).await
    }
}

async fn receiver(status: u16) -> GraphqlHttpServer {
    GraphqlHttpServer::start(move |_: &RecordedRequest| {
        Reply::raw(status, "application/json", "{}")
    })
    .await
}

/// monitors "Slack webhook": a Slack-compatible payload naming the
/// monitor, the rule, the environment and the latest failure is POSTed.
#[tokio::test]
async fn slack_webhooks_post_a_slack_payload() {
    let server = receiver(200).await;
    let fx = Fixture::new(&server.url(&format!("/hook/{HOOK_CANARY}")));
    let channels = [webhook("{{slack_hook}}", WebhookTemplate::Slack)];
    let deliveries = fx.send(&channels, &firing()).await;
    assert_eq!(
        deliveries,
        [Delivery {
            channel: "webhook (slack)".into(),
            error: None
        }]
    );
    let hits = server.hits();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].method, "POST");
    assert_eq!(hits[0].header("content-type"), Some("application/json"));
    let payload = hits[0].json();
    assert_eq!(
        payload["text"],
        "[FIRING] api-health: consecutive_failures: 3"
    );
    let fields = payload["attachments"][0]["fields"].to_string();
    for want in [
        "api-health",
        "prod",
        "consecutive_failures: 3",
        "health/status",
    ] {
        assert!(fields.contains(want), "{want}: {fields}");
    }
}

#[tokio::test]
async fn discord_and_generic_templates_render_their_shapes() {
    let server = receiver(204).await;
    let fx = Fixture::new(&server.url("/hook"));
    let channels = [
        webhook("{{slack_hook}}", WebhookTemplate::Discord),
        webhook("{{plain_hook}}", WebhookTemplate::Generic),
    ];
    let deliveries = fx.send(&channels, &firing()).await;
    assert!(deliveries.iter().all(Delivery::is_ok), "{deliveries:?}");
    let hits = server.hits();
    let discord = hits
        .iter()
        .map(RecordedRequest::json)
        .find(|j| j["embeds"].is_array());
    let discord = discord.expect("a discord payload");
    assert_eq!(discord["embeds"][0]["fields"][1]["value"], "prod");
    let generic = hits
        .iter()
        .map(RecordedRequest::json)
        .find(|j| j["status"].is_string());
    let generic = generic.expect("a generic payload");
    assert_eq!(generic["status"], "firing");
    assert_eq!(generic["monitor"], "api-health");
    assert_eq!(generic["at"], "2026-03-01T10:00:00Z");
}

#[tokio::test]
async fn workspace_templates_override_the_builtin_ones() {
    let server = receiver(200).await;
    let fx = Fixture::new(&server.url("/hook"));
    let dir = fx.root.path().join(TEMPLATES_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("generic.json.j2"),
        "{\"summary\": {{ title | tojson }}, \"why\": {{ failure | tojson }}}",
    )
    .unwrap();
    let channels = [webhook("{{slack_hook}}", WebhookTemplate::Generic)];
    assert!(fx.send(&channels, &firing()).await[0].is_ok());
    let payload = server.hits()[0].json();
    assert_eq!(
        payload["summary"],
        "[FIRING] api-health: consecutive_failures: 3"
    );
    // The override renders a secret: the redaction still applies.
    assert!(!payload.to_string().contains(TOKEN), "{payload}");
    std::fs::write(dir.join("generic.json.j2"), "not json {{ title }}").unwrap();
    let delivery = &fx.send(&channels, &firing()).await[0];
    let error = delivery.error.as_deref().unwrap();
    assert!(
        error.contains("template generic did not render JSON"),
        "{error}"
    );
}

/// monitors "One channel down": the webhook answers 500, the desktop
/// notification is still shown, and the webhook failure is reported after
/// one retry.
#[tokio::test]
async fn one_channel_down_does_not_stop_the_others() {
    let server = receiver(500).await;
    let fx = Fixture::new(&server.url("/hook"));
    let channels = [
        Channel::Desktop,
        webhook("{{slack_hook}}", WebhookTemplate::Slack),
    ];
    let deliveries = fx.send(&channels, &firing()).await;
    assert!(deliveries[0].is_ok());
    assert_eq!(deliveries[1].error.as_deref(), Some("HTTP 500"));
    assert_eq!(server.hits().len(), 2, "one retry");
    let shown = fx.desktop.shown.lock().unwrap().clone();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].0, "[FIRING] api-health: consecutive_failures: 3");
    // A failing desktop does not stop the webhook either.
    let ok = receiver(200).await;
    let mut fx = Fixture::new(&ok.url("/"));
    fx.desktop.fail = true;
    let deliveries = fx.send(&channels, &firing()).await;
    assert_eq!(
        deliveries[0].error.as_deref(),
        Some("no notification server")
    );
    assert!(deliveries[1].is_ok());
}

#[tokio::test]
async fn a_stalled_webhook_times_out_and_is_retried_once() {
    let server = TestServer::stalled("http").await;
    let fx = Fixture::new(&server.url("/hook"));
    let channels = [webhook("{{slack_hook}}", WebhookTemplate::Generic)];
    let started = std::time::Instant::now();
    let deliveries = fx
        .notifier(Duration::from_millis(300))
        .send(&channels, &firing())
        .await;
    let error = deliveries[0].error.as_deref().unwrap();
    assert!(
        error.contains("timed out") || error.contains("timeout"),
        "{error}"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(600),
        "two attempts"
    );
}

/// Canary: a secret in the failure message never reaches the payload or
/// the desktop, and a secret webhook URL never appears in an error.
#[tokio::test]
async fn secrets_and_webhook_urls_are_redacted_everywhere() {
    let server = receiver(200).await;
    let fx = Fixture::new(&server.url(&format!("/hook/{HOOK_CANARY}")));
    let channels = [
        Channel::Desktop,
        webhook("{{slack_hook}}", WebhookTemplate::Slack),
    ];
    let deliveries = fx.send(&channels, &firing()).await;
    assert!(deliveries.iter().all(Delivery::is_ok), "{deliveries:?}");
    let body = server.hits()[0].body_text();
    assert!(!body.contains(TOKEN), "{body}");
    assert!(body.contains("«redacted:token»"), "{body}");
    let shown = format!("{:?}", fx.desktop.shown.lock().unwrap());
    assert!(
        !shown.contains(TOKEN) && shown.contains("«redacted:token»"),
        "{shown}"
    );

    // An unreachable webhook, from a secret or a plain variable: the error
    // names neither the URL nor its path.
    let closed = format!("http://127.0.0.1:1/hook/{HOOK_CANARY}");
    for hook in ["{{slack_hook}}", "{{plain_hook}}"] {
        let fx = Fixture::new(&closed);
        let deliveries = fx
            .notifier(Duration::from_secs(5))
            .send(&[webhook(hook, WebhookTemplate::Slack)], &firing())
            .await;
        let shown = format!("{deliveries:?}");
        assert!(deliveries[0].error.is_some(), "{shown}");
        assert!(!shown.contains(HOOK_CANARY), "{shown}");
        assert!(!shown.contains("127.0.0.1:1/hook"), "{shown}");
    }
}

/// A webhook that redirects is a failure: the alert is not replayed to
/// the redirect target (no SSRF past the configured URL).
#[tokio::test]
async fn a_redirecting_webhook_fails_and_is_not_followed() {
    let internal = receiver(200).await;
    let redirector = TestServer::http().await;
    for code in [307, 308, 302] {
        let target = internal.url("/admin/internal");
        let hook = redirector.url(&format!("/redirect/{code}?to={target}"));
        let fx = Fixture::new(&hook);
        let channels = [webhook("{{slack_hook}}", WebhookTemplate::Slack)];
        let deliveries = fx.send(&channels, &firing()).await;
        let expected = format!("HTTP {code}");
        assert_eq!(deliveries[0].error.as_deref(), Some(expected.as_str()));
    }
    assert!(internal.hits().is_empty(), "the redirect was followed");
}

/// `[defaults] verify_tls = false` is for requests: webhook delivery still
/// verifies the certificate.
#[tokio::test]
async fn webhooks_verify_tls_even_when_the_workspace_does_not() {
    let certs = rustman_test_support::TestCerts::generate();
    let server = TestServer::https(&certs, rustman_test_support::Tls::SelfSigned).await;
    let mut fx = Fixture::new(&server.url("/status/200"));
    fx.settings = Settings::parse(
        Path::new("rustman.toml"),
        "[workspace]\nname = \"t\"\nversion = 1\n\n[defaults]\nverify_tls = false\n",
    )
    .unwrap()
    .0;
    assert!(!fx.settings.defaults.verify_tls);
    let channels = [webhook("{{slack_hook}}", WebhookTemplate::Slack)];
    let deliveries = fx.send(&channels, &firing()).await;
    let error = deliveries[0].error.as_deref().expect("a TLS failure");
    assert!(
        error.to_lowercase().contains("tls") || error.contains("certificate"),
        "{error}"
    );
}

#[tokio::test]
async fn an_unset_secret_fails_only_its_channel() {
    let fx = Fixture::new("http://127.0.0.1:1/");
    let channels = [
        webhook("{{missing_hook}}", WebhookTemplate::Slack),
        Channel::Desktop,
    ];
    let deliveries = fx.send(&channels, &firing()).await;
    let error = deliveries[0].error.as_deref().unwrap();
    assert!(error.contains("missing_hook"), "{error}");
    assert!(deliveries[1].is_ok());
}

#[cfg(not(feature = "smtp"))]
#[tokio::test]
async fn email_needs_the_smtp_feature() {
    let fx = Fixture::new("http://127.0.0.1:1/");
    let email = crate::alerts::EmailChannel {
        host: "smtp.example.com".into(),
        port: 587,
        from: "a@example.com".into(),
        to: vec!["b@example.com".into()],
        username: None,
        password: None,
        tls: crate::alerts::EmailTls::Starttls,
    };
    let deliveries = fx.send(&[Channel::Email(email)], &firing()).await;
    assert_eq!(deliveries[0].channel, "email (smtp.example.com)");
    let error = deliveries[0].error.as_deref().unwrap();
    assert_eq!(
        error,
        "email notifications need rustman built with the smtp feature"
    );
}

#[test]
fn alerts_read_naturally() {
    let mut alert = firing();
    assert_eq!(
        alert.text(),
        format!(
            "api-health (prod): consecutive_failures: 3 is firing.\nLatest failure: health/status: header x-token was {TOKEN}"
        )
    );
    alert.kind = AlertKind::Resolved;
    alert.failure = None;
    alert.duration = Some(Duration::from_secs(42 * 60));
    assert_eq!(
        alert.title(),
        "[RESOLVED] api-health: consecutive_failures: 3"
    );
    assert_eq!(
        alert.text(),
        "api-health (prod): consecutive_failures: 3 resolved; the outage lasted 42 minutes."
    );
    alert.kind = AlertKind::Reminder;
    assert!(alert.text().contains("has been firing for 42 minutes"));
    alert.kind = AlertKind::Test;
    alert.rule = None;
    assert_eq!(alert.title(), "[TEST] api-health");
}
