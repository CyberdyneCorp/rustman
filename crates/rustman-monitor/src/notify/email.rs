//! The email channel: SMTP with STARTTLS or implicit TLS through `lettre`
//! on rustls (the `smtp` cargo feature). Plain-text SMTP is never used:
//! a server that does not offer STARTTLS fails the delivery. Certificates
//! are checked against the OS roots plus `[http] ca_bundle`.

use rustman_core::Redactor;
use rustman_core::vars::Resolver;

use super::{Alert, Notifier};
use crate::alerts::EmailChannel;

#[cfg(not(feature = "smtp"))]
pub(super) async fn send(
    _notifier: &Notifier<'_>,
    _email: &EmailChannel,
    _alert: &Alert,
    _resolver: &Resolver<'_>,
    _redactor: &mut Redactor,
) -> Result<(), String> {
    Err("email notifications need rustman built with the smtp feature".to_owned())
}

#[cfg(feature = "smtp")]
pub(super) use smtp::send;

#[cfg(feature = "smtp")]
mod smtp {
    use lettre::message::{Mailbox, Message, header::ContentType};
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::transport::smtp::client::{Certificate, Tls, TlsParameters};
    use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

    use super::*;
    use crate::alerts::EmailTls;
    use crate::notify::{resolve, retry};

    pub(in crate::notify) async fn send(
        notifier: &Notifier<'_>,
        email: &EmailChannel,
        alert: &Alert,
        resolver: &Resolver<'_>,
        redactor: &mut Redactor,
    ) -> Result<(), String> {
        let mut field = |name: &str, text: &str, secret: bool| {
            resolve(resolver, redactor, name, text, secret).map(|r| r.text)
        };
        let host = field("email host", &email.host, false)?;
        let from = field("email from", &email.from, false)?;
        let to = email
            .to
            .iter()
            .map(|to| field("email to", to, false))
            .collect::<Result<Vec<_>, _>>()?;
        let username = email
            .username
            .as_deref()
            .map(|u| field("email username", u, false))
            .transpose()?;
        let password = email
            .password
            .as_deref()
            .map(|p| field("email password", p, true))
            .transpose()?;
        let message = message(&from, &to, alert)?;
        let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&host)
            .port(email.port)
            .tls(tls(notifier, &host, email.tls)?)
            .timeout(Some(notifier.timeout));
        if let Some(username) = username {
            let password = password.unwrap_or_default();
            builder = builder.credentials(Credentials::new(username, password));
        }
        let transport = builder.build();
        retry(notifier.timeout, || async {
            transport
                .send(message.clone())
                .await
                .map(drop)
                .map_err(|e| format!("smtp: {e}"))
        })
        .await
    }

    fn message(from: &str, to: &[String], alert: &Alert) -> Result<Message, String> {
        let mailbox = |text: &str| {
            text.parse::<Mailbox>()
                .map_err(|e| format!("invalid email address '{text}': {e}"))
        };
        let mut builder = Message::builder()
            .from(mailbox(from)?)
            .subject(alert.title())
            .header(ContentType::TEXT_PLAIN);
        for to in to {
            builder = builder.to(mailbox(to)?);
        }
        builder
            .body(alert.text())
            .map_err(|e| format!("cannot build the email: {e}"))
    }

    /// STARTTLS required, or TLS from the first byte; the OS roots plus
    /// `[http] ca_bundle`.
    fn tls(notifier: &Notifier<'_>, host: &str, mode: EmailTls) -> Result<Tls, String> {
        let mut params = TlsParameters::builder(host.to_owned());
        if let Some(bundle) = &notifier.settings.http.ca_bundle {
            let pem = std::fs::read(notifier.root.join(bundle))
                .map_err(|e| format!("cannot read ca_bundle {bundle}: {e}"))?;
            let certs = Certificate::from_pem(&pem)
                .map_err(|e| format!("invalid ca_bundle {bundle}: {e}"))?;
            params = params.add_root_certificate(certs);
        }
        let params = params.build_rustls().map_err(|e| format!("tls: {e}"))?;
        Ok(match mode {
            EmailTls::Starttls => Tls::Required(params),
            EmailTls::Tls => Tls::Wrapper(params),
        })
    }
}
