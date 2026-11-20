//! The `alerts:` block of a monitor file (monitors "Alert rules",
//! "Notification channels"): the rules and where their notifications go.
//! Only the model and its validation live here.
//!
//! ```yaml
//! alerts:
//!   cooldown: 1h
//!   rules:
//!     - consecutive_failures: 3
//!     - failures_in_window: {count: 5, window: 1h}
//!     - latency_p95_gt: 800
//!       window: 15m
//!     - assertion_failed: health/status
//!     - status_not: 2xx
//!   notify:
//!     - desktop: true
//!     - webhook: {url: "{{slack_hook}}", template: slack}
//!     - email: {host: smtp.example.com, port: 587, from: a@example.com,
//!               to: [ops@example.com], username: "{{smtp_user}}",
//!               password: "{{smtp_password}}", tls: starttls}
//! ```

use std::path::Path;
use std::time::Duration;

use rustman_core::LoadError;
use rustman_core::format::Located;
use serde::Deserialize;

use crate::definition::is_reference;
use crate::duration;

/// The default `consecutive_failures` threshold, and the rule used when
/// `alerts.rules` is not set.
pub const DEFAULT_CONSECUTIVE_FAILURES: u32 = 2;
/// The default window of `latency_p95_gt`.
pub const DEFAULT_LATENCY_WINDOW: Duration = Duration::from_secs(15 * 60);
/// The default reminder interval of a rule that keeps firing.
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(3600);

/// A monitor's alert rules and channels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alerts {
    pub rules: Vec<Rule>,
    pub notify: Vec<Channel>,
    pub cooldown: Duration,
}

impl Default for Alerts {
    fn default() -> Self {
        Self {
            rules: vec![Rule::ConsecutiveFailures(DEFAULT_CONSECUTIVE_FAILURES)],
            notify: Vec::new(),
            cooldown: DEFAULT_COOLDOWN,
        }
    }
}

/// One alert rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rule {
    /// The last N runs failed or errored.
    ConsecutiveFailures(u32),
    /// At least `count` failed or errored runs within `window`.
    FailuresInWindow { count: u32, window: Duration },
    /// The p95 latency of the runs within `window` is above `threshold_ms`.
    LatencyP95 { threshold_ms: u64, window: Duration },
    /// The assertions of this request (its id) failed in the last run.
    AssertionFailed(String),
    /// A response status of the last run is not this code or class.
    StatusNot(StatusMatch),
}

/// The rule as written in the file, such as `consecutive_failures: 3`; it
/// also keys the rule's alert state.
impl std::fmt::Display for Rule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::duration::format as d;
        match self {
            Self::ConsecutiveFailures(n) => write!(f, "consecutive_failures: {n}"),
            Self::FailuresInWindow { count, window } => {
                write!(f, "failures_in_window: {count} in {}", d(*window))
            }
            Self::LatencyP95 {
                threshold_ms,
                window,
            } => write!(f, "latency_p95_gt: {threshold_ms} over {}", d(*window)),
            Self::AssertionFailed(request) => write!(f, "assertion_failed: {request}"),
            Self::StatusNot(status) => write!(f, "status_not: {status}"),
        }
    }
}

impl std::fmt::Display for StatusMatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code(code) => write!(f, "{code}"),
            Self::Class(class) => write!(f, "{class}xx"),
        }
    }
}

/// `status_not: 200` or `status_not: 2xx`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusMatch {
    Code(u16),
    /// The first digit of a class: `2` for `2xx`.
    Class(u16),
}

impl StatusMatch {
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let invalid =
            || format!("invalid status '{text}' (use a code such as 200 or a class such as 2xx)");
        if let Some(digit) = text.strip_suffix("xx") {
            return match digit.parse::<u16>() {
                Ok(d @ 1..=5) if digit.len() == 1 => Ok(Self::Class(d)),
                _ => Err(invalid()),
            };
        }
        match text.parse::<u16>() {
            Ok(code @ 100..=599) => Ok(Self::Code(code)),
            _ => Err(invalid()),
        }
    }

    pub fn matches(self, status: u16) -> bool {
        match self {
            Self::Code(code) => status == code,
            Self::Class(class) => status / 100 == class,
        }
    }
}

/// Where alerts go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Channel {
    /// An OS notification.
    Desktop,
    /// A JSON POST; `url` is a `{{secret}}` reference.
    Webhook {
        url: String,
        template: WebhookTemplate,
    },
    Email(EmailChannel),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebhookTemplate {
    Slack,
    Discord,
    #[default]
    Generic,
}

/// SMTP over TLS; credentials are `{{secret}}` references.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailChannel {
    pub host: String,
    pub port: u16,
    pub from: String,
    pub to: Vec<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub tls: EmailTls,
}

/// How SMTP is secured; plain-text SMTP is not offered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmailTls {
    #[default]
    Starttls,
    Tls,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AlertsFile {
    rules: Option<Vec<Located<RuleFile>>>,
    notify: Vec<Located<NotifyFile>>,
    cooldown: Option<Located<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleFile {
    #[serde(default)]
    consecutive_failures: Option<u32>,
    #[serde(default)]
    failures_in_window: Option<WindowCount>,
    #[serde(default)]
    latency_p95_gt: Option<u64>,
    #[serde(default)]
    window: Option<String>,
    #[serde(default)]
    assertion_failed: Option<String>,
    #[serde(default)]
    status_not: Option<CodeOrText>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowCount {
    count: u32,
    window: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CodeOrText {
    Code(u16),
    Text(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NotifyFile {
    #[serde(default)]
    desktop: Option<bool>,
    #[serde(default)]
    webhook: Option<WebhookFile>,
    #[serde(default)]
    email: Option<EmailChannel>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebhookFile {
    url: String,
    #[serde(default)]
    template: WebhookTemplate,
}

impl AlertsFile {
    pub(crate) fn validate(self, file: &Path) -> Result<Alerts, LoadError> {
        let err = |raw: Option<rustman_core::Position>, m: String| LoadError::new(file, raw, m);
        let cooldown = match &self.cooldown {
            Some(c) => duration::parse(c).map_err(|m| err(c.position(), m))?,
            None => DEFAULT_COOLDOWN,
        };
        let rules = match self.rules {
            Some(rules) => rules
                .into_iter()
                .map(|r| rule(&r).map_err(|m| err(r.position(), m)))
                .collect::<Result<_, _>>()?,
            None => Alerts::default().rules,
        };
        let notify = self
            .notify
            .into_iter()
            .map(|n| channel(&n).map_err(|m| err(n.position(), m)))
            .collect::<Result<_, _>>()?;
        Ok(Alerts {
            rules,
            notify,
            cooldown,
        })
    }
}

/// The one rule an entry sets; `window` goes with `latency_p95_gt` only.
fn rule(raw: &RuleFile) -> Result<Rule, String> {
    let set = [
        raw.consecutive_failures.is_some(),
        raw.failures_in_window.is_some(),
        raw.latency_p95_gt.is_some(),
        raw.assertion_failed.is_some(),
        raw.status_not.is_some(),
    ];
    if set.iter().filter(|s| **s).count() != 1 {
        return Err("a rule must set exactly one of consecutive_failures, failures_in_window, latency_p95_gt, assertion_failed or status_not".to_owned());
    }
    if raw.window.is_some() && raw.latency_p95_gt.is_none() {
        return Err("window applies to latency_p95_gt only".to_owned());
    }
    if let Some(n) = raw.consecutive_failures {
        return positive(n, "consecutive_failures").map(Rule::ConsecutiveFailures);
    }
    if let Some(w) = &raw.failures_in_window {
        let count = positive(w.count, "failures_in_window.count")?;
        let window = duration::parse(&w.window)?;
        return Ok(Rule::FailuresInWindow { count, window });
    }
    if let Some(threshold_ms) = raw.latency_p95_gt {
        let window = match &raw.window {
            Some(w) => duration::parse(w)?,
            None => DEFAULT_LATENCY_WINDOW,
        };
        return Ok(Rule::LatencyP95 {
            threshold_ms,
            window,
        });
    }
    if let Some(request) = &raw.assertion_failed {
        return Ok(Rule::AssertionFailed(request.trim().to_owned()));
    }
    let status = match raw.status_not.as_ref() {
        Some(CodeOrText::Code(code)) => StatusMatch::parse(&code.to_string())?,
        Some(CodeOrText::Text(text)) => StatusMatch::parse(text)?,
        None => unreachable!("exactly one rule key is set"),
    };
    Ok(Rule::StatusNot(status))
}

fn positive(n: u32, field: &str) -> Result<u32, String> {
    if n == 0 {
        Err(format!("{field} must be at least 1"))
    } else {
        Ok(n)
    }
}

/// The one channel an entry sets, with its credentials as references.
fn channel(raw: &NotifyFile) -> Result<Channel, String> {
    let set = [
        raw.desktop.is_some(),
        raw.webhook.is_some(),
        raw.email.is_some(),
    ];
    if set.iter().filter(|s| **s).count() != 1 {
        return Err("a notify entry must set exactly one of desktop, webhook or email".to_owned());
    }
    if let Some(desktop) = raw.desktop {
        return if desktop {
            Ok(Channel::Desktop)
        } else {
            Err("desktop: false is not a channel; remove the entry".to_owned())
        };
    }
    if let Some(webhook) = &raw.webhook {
        if !is_reference(&webhook.url) {
            return Err("webhook url must reference a secret ({{name}}), not be written in the monitor file".to_owned());
        }
        return Ok(Channel::Webhook {
            url: webhook.url.trim().to_owned(),
            template: webhook.template,
        });
    }
    let email = raw.email.clone().expect("exactly one channel key is set");
    email_channel(email).map(Channel::Email)
}

fn email_channel(email: EmailChannel) -> Result<EmailChannel, String> {
    if email.to.is_empty() {
        return Err("email to must list at least one address".to_owned());
    }
    let literal = |v: &Option<String>| v.as_deref().is_some_and(|v| !is_reference(v));
    if literal(&email.password) {
        return Err(
            "email password must reference a secret ({{name}}), not be written in the monitor file"
                .to_owned(),
        );
    }
    Ok(email)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::parse;

    const HEAD: &str = "version: 1
name: api-health
targets: [health/]
environment: prod
schedule: {every: 5m}
";

    fn alerts(block: &str) -> Result<Alerts, String> {
        let text = format!("{HEAD}alerts:\n{block}");
        parse(Path::new("api-health.rman-monitor.yaml"), &text)
            .map(|m| m.alerts)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn every_rule_kind_parses() {
        let parsed = alerts(
            "  cooldown: 30m
  rules:
    - consecutive_failures: 3
    - failures_in_window: {count: 5, window: 1h}
    - latency_p95_gt: 800
      window: 15m
    - latency_p95_gt: 500
    - assertion_failed: health/status
    - status_not: 2xx
    - status_not: 503
",
        )
        .unwrap();
        assert_eq!(parsed.cooldown, Duration::from_secs(1800));
        let fifteen = Duration::from_secs(900);
        assert_eq!(
            parsed.rules,
            [
                Rule::ConsecutiveFailures(3),
                Rule::FailuresInWindow {
                    count: 5,
                    window: Duration::from_secs(3600)
                },
                Rule::LatencyP95 {
                    threshold_ms: 800,
                    window: fifteen
                },
                Rule::LatencyP95 {
                    threshold_ms: 500,
                    window: fifteen
                },
                Rule::AssertionFailed("health/status".into()),
                Rule::StatusNot(StatusMatch::Class(2)),
                Rule::StatusNot(StatusMatch::Code(503)),
            ]
        );
    }

    #[test]
    fn bad_rules_name_their_line() {
        for (block, want) in [
            (
                "  rules:\n    - consecutive_failures: 0\n",
                "api-health.rman-monitor.yaml:8: consecutive_failures must be at least 1",
            ),
            (
                "  rules:\n    - {consecutive_failures: 2, latency_p95_gt: 3}\n",
                "exactly one of",
            ),
            ("  rules:\n    - {status_not: 2x}\n", "invalid status '2x'"),
            (
                "  rules:\n    - {status_not: 700}\n",
                "invalid status '700'",
            ),
            (
                "  rules:\n    - {consecutive_failures: 2, window: 5m}\n",
                "window applies to latency_p95_gt only",
            ),
            ("  cooldown: soon\n", "invalid duration"),
        ] {
            let err = alerts(block).unwrap_err();
            assert!(err.contains(want), "{block}: {err}");
        }
    }

    #[test]
    fn channels_parse_with_secret_references() {
        let parsed = alerts(
            "  notify:
    - desktop: true
    - webhook: {url: \"{{slack_hook}}\", template: slack}
    - webhook: {url: \"{{hook}}\"}
    - email: {host: smtp.example.com, port: 465, from: a@example.com, to: [ops@example.com], username: me, password: \"{{smtp_password}}\", tls: tls}
",
        )
        .unwrap();
        assert_eq!(parsed.rules, [Rule::ConsecutiveFailures(2)]);
        assert_eq!(parsed.notify.len(), 4);
        assert_eq!(parsed.notify[0], Channel::Desktop);
        assert_eq!(
            parsed.notify[2],
            Channel::Webhook {
                url: "{{hook}}".into(),
                template: WebhookTemplate::Generic
            }
        );
        let Channel::Email(email) = &parsed.notify[3] else {
            panic!("not an email channel");
        };
        assert_eq!(email.tls, EmailTls::Tls);
        assert_eq!(email.port, 465);
    }

    /// Canary: a literal webhook URL or SMTP password in a monitor file is
    /// refused, and the error never echoes it.
    #[test]
    fn literal_credentials_are_refused() {
        let secret = "https://hooks.slack.com/services/T0/B0/canary-s3cret";
        let err = alerts(&format!(
            "  notify:\n    - webhook: {{url: \"{secret}\"}}\n"
        ))
        .unwrap_err();
        assert!(err.contains("must reference a secret"), "{err}");
        assert!(!err.contains("canary-s3cret"), "{err}");
        let err = alerts(
            "  notify:\n    - email: {host: h, port: 587, from: a@b, to: [c@d], password: canary-pw}\n",
        )
        .unwrap_err();
        assert!(err.contains("must reference a secret"), "{err}");
        assert!(!err.contains("canary-pw"), "{err}");
        for (block, want) in [
            ("  notify:\n    - desktop: false\n", "desktop: false"),
            ("  notify:\n    - {}\n", "exactly one of desktop"),
            (
                "  notify:\n    - email: {host: h, port: 587, from: a@b, to: []}\n",
                "at least one address",
            ),
            (
                "  notify:\n    - webhook: {url: \"{{h}}\", template: teams}\n",
                "unknown variant",
            ),
        ] {
            let err = alerts(block).unwrap_err();
            assert!(err.contains(want), "{block}: {err}");
        }
    }

    #[test]
    fn status_matches() {
        assert!(StatusMatch::Class(2).matches(204));
        assert!(!StatusMatch::Class(2).matches(500));
        assert!(StatusMatch::Code(503).matches(503));
        assert!(!StatusMatch::Code(503).matches(502));
    }
}
