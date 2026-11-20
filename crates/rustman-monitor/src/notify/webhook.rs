//! The webhook channel: a JSON POST through the HTTP engine (proxies and
//! `[http] ca_bundle` apply as for requests). A response outside 2xx is a
//! failure, redirects included: the alert is never replayed to a host the
//! user did not configure. TLS is always verified, whatever the workspace's
//! `[defaults] verify_tls` says, and plain `http://` is refused except to a
//! loopback host. The URL is a secret: it is redacted from every error.

use rustman_core::Redactor;
use rustman_core::model::RequestSettings;
use rustman_core::vars::{
    Resolved, ResolvedBody, ResolvedPair, ResolvedRequest, Resolver, TlsFiles,
};
use rustman_http::SendOptions;

use super::{Alert, Notifier, resolve, retry, templates};
use crate::alerts::WebhookTemplate;

pub(super) struct Webhook<'a> {
    pub url: &'a str,
    pub template: WebhookTemplate,
}

impl Webhook<'_> {
    pub async fn send(
        &self,
        notifier: &Notifier<'_>,
        alert: &Alert,
        resolver: &Resolver<'_>,
        redactor: &mut Redactor,
    ) -> Result<(), String> {
        let url = resolve(resolver, redactor, "webhook url", self.url, true)?;
        check_scheme(&url.text)?;
        let payload = templates::render(notifier.root, self.template, alert, redactor)?;
        let name = super::reference_name(self.url).unwrap_or("webhook_url");
        let request = post(url, &payload, name);
        let mut options = SendOptions::for_request(notifier.settings, notifier.root, &request)
            .map_err(|e| e.to_string())?;
        options.timeout = notifier.timeout;
        retry(notifier.timeout, || async {
            let exchange = notifier
                .engine
                .send(&request, &options)
                .await
                .map_err(|e| e.to_string())?;
            if (200..300).contains(&exchange.status) {
                Ok(())
            } else {
                Err(format!("HTTP {}", exchange.status))
            }
        })
        .await
    }
}

/// Only `https://`, or `http://` to a loopback host: the URL often carries a
/// capability token and the payload names the failure, so neither goes out
/// in clear over the network. The error never shows the URL.
fn check_scheme(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|_| "webhook url is not a valid URL".to_owned())?;
    let loopback = match parsed.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if loopback => Ok(()),
        "http" => Err("webhook url must use https (http is allowed only to localhost)".to_owned()),
        other => Err(format!("webhook url scheme `{other}` is not supported")),
    }
}

/// `POST <url>` with `payload` as JSON. The URL is recorded as a secret
/// span, so the engine's own errors redact it too.
fn post(url: Resolved, payload: &serde_json::Value, name: &str) -> ResolvedRequest {
    let secret = rustman_core::Secret::new(name, url.text.clone());
    let span = rustman_core::vars::SecretSpan {
        name: name.to_owned(),
        range: 0..url.text.len(),
    };
    ResolvedRequest {
        method: "POST".to_owned(),
        url: Resolved {
            text: url.text,
            secret_spans: vec![span],
        },
        query: Vec::new(),
        headers: vec![ResolvedPair {
            name: Resolved::literal("Content-Type"),
            value: Resolved::literal("application/json"),
        }],
        body: ResolvedBody::JsonText(Resolved::literal(payload.to_string())),
        auth: None,
        settings: delivery_settings(),
        tls: TlsFiles::default(),
        secrets: vec![secret],
    }
}

/// Never follow a redirect and always verify TLS: the workspace's
/// `[defaults]` are for the requests under test, not for alert delivery.
fn delivery_settings() -> RequestSettings {
    RequestSettings {
        follow_redirects: Some(false),
        verify_tls: Some(true),
        ..RequestSettings::default()
    }
}

#[cfg(test)]
mod tests {
    use super::check_scheme;

    #[test]
    fn webhooks_need_https_except_to_a_loopback_host() {
        for ok in [
            "https://hooks.example.com/T0/abc",
            "http://127.0.0.1:8080/hook",
            "http://localhost/hook",
            "http://[::1]:9/hook",
        ] {
            assert_eq!(check_scheme(ok), Ok(()), "{ok}");
        }
        let error = check_scheme("http://hooks.example.com/canary-path").unwrap_err();
        assert!(error.contains("must use https"), "{error}");
        assert!(!error.contains("canary-path"), "{error}");
        assert!(check_scheme("ftp://hooks.example.com/x").is_err());
        assert!(check_scheme("not a url").is_err());
    }
}
