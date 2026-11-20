//! Notification channels (monitors "Notification channels", "Redaction in
//! alerts and results", design D5): a desktop notification, a webhook POST
//! through the HTTP engine, and email over SMTP with TLS.
//!
//! Every channel gets [`TIMEOUT`] per attempt and one retry. A channel that
//! fails is reported in its [`Delivery`] and never stops the others. Every
//! title, text, payload and error goes through the [`Redactor`], which
//! knows the values of the environment's secrets and of the channel's own
//! credentials, so neither a secret in a failure message nor the webhook
//! URL is ever sent or shown in clear.

mod alert;
pub mod desktop;
mod email;
mod templates;
mod webhook;

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use futures_util::future::join_all;
use rustman_core::secret::Secret;
use rustman_core::settings::Settings;
use rustman_core::vars::{
    EnvironmentSource, Layer, Lookup, Resolved, Resolver, SecretLookup, VariablesSource,
};
use rustman_core::{Environment, Redactor};
use rustman_http::HttpEngine;

use crate::alerts::Channel;

pub use alert::{Alert, AlertKind};
pub use desktop::{Desktop, SystemDesktop};
pub use templates::{TEMPLATES_DIR, render};

/// The time one attempt of one channel may take.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// How one channel did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    /// `desktop`, `webhook (slack)`, `email (smtp.example.com)`: never a
    /// credential.
    pub channel: String,
    /// Why it failed (redacted); `None` when it was delivered.
    pub error: Option<String>,
}

impl Delivery {
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}

/// What sending needs: the workspace, the monitor's environment (its
/// variables and secrets resolve the channels' references) and the
/// desktop to show notifications on.
pub struct Notifier<'a> {
    pub root: &'a Path,
    pub settings: &'a Settings,
    pub environment: &'a Environment,
    pub secrets: &'a dyn SecretLookup,
    pub desktop: &'a dyn Desktop,
    pub engine: &'a HttpEngine,
    /// [`TIMEOUT`] outside tests.
    pub timeout: Duration,
}

impl Notifier<'_> {
    /// Send `alert` through every channel, all at once; one delivery per
    /// channel, in order.
    pub async fn send(&self, channels: &[Channel], alert: &Alert) -> Vec<Delivery> {
        let resolver = self.resolver();
        let redactor = self.redactor(&resolver);
        let alert = alert.redacted(&redactor);
        let sends = channels
            .iter()
            .map(|channel| self.deliver(channel, &alert, &resolver, redactor.clone()));
        join_all(sends).await
    }

    async fn deliver(
        &self,
        channel: &Channel,
        alert: &Alert,
        resolver: &Resolver<'_>,
        mut redactor: Redactor,
    ) -> Delivery {
        let result = match channel {
            Channel::Desktop => {
                let (title, text) = (alert.title(), alert.text());
                retry(self.timeout, || async { self.desktop.show(&title, &text) }).await
            }
            Channel::Webhook { url, template } => {
                let hook = webhook::Webhook {
                    url,
                    template: *template,
                };
                hook.send(self, alert, resolver, &mut redactor).await
            }
            Channel::Email(email) => email::send(self, email, alert, resolver, &mut redactor).await,
        };
        Delivery {
            channel: label(channel),
            error: result.err().map(|e| redactor.text(&e).into_owned()),
        }
    }

    /// The environment, then `rustman.toml` variables.
    fn resolver(&self) -> Resolver<'_> {
        Resolver::new()
            .with(EnvironmentSource::new(self.environment, self.secrets))
            .with(VariablesSource::new(
                Layer::Settings,
                &self.settings.variables,
            ))
    }

    /// A redactor for every secret of the environment that has a value.
    fn redactor(&self, resolver: &Resolver<'_>) -> Redactor {
        let mut redactor = Redactor::new();
        for name in &self.environment.secrets {
            if let Some(Lookup::Secret(secret)) = resolver.lookup(name) {
                redactor.add(&secret);
            }
        }
        redactor
    }
}

/// How a channel is named in output: never its URL or credentials.
pub fn label(channel: &Channel) -> String {
    match channel {
        Channel::Desktop => "desktop".to_owned(),
        Channel::Webhook { template, .. } => {
            let name = match template {
                crate::alerts::WebhookTemplate::Slack => "slack",
                crate::alerts::WebhookTemplate::Discord => "discord",
                crate::alerts::WebhookTemplate::Generic => "generic",
            };
            format!("webhook ({name})")
        }
        Channel::Email(email) => format!("email ({})", email.host),
    }
}

/// Resolve a channel field (a `{{name}}` reference or a literal) and teach
/// `redactor` its value under the referenced name when `secret`, plus
/// every secret it used.
fn resolve(
    resolver: &Resolver<'_>,
    redactor: &mut Redactor,
    field: &str,
    text: &str,
    secret: bool,
) -> Result<Resolved, String> {
    let mut interpolator = resolver.interpolator();
    let resolved = interpolator.text(text);
    let (resolved, used) = interpolator
        .finish(resolved)
        .map_err(|e| format!("{field}: {e}"))?;
    for value in &used {
        redactor.add(value);
    }
    if secret {
        let name = reference_name(text).unwrap_or(field);
        redactor.add(&Secret::new(name, resolved.text.clone()));
    }
    Ok(resolved)
}

/// `slack_hook` for `{{slack_hook}}`.
fn reference_name(text: &str) -> Option<&str> {
    let inner = text.trim().strip_prefix("{{")?.strip_suffix("}}")?;
    Some(inner.trim())
}

/// Run `attempt` with `timeout`, once more when it fails.
async fn retry<F, Fut>(timeout: Duration, mut attempt: F) -> Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let mut last = String::new();
    for _ in 0..2 {
        match tokio::time::timeout(timeout, attempt()).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(e)) => last = e,
            Err(_) => {
                last = format!("timed out after {}", crate::duration::format(timeout));
            }
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests;
