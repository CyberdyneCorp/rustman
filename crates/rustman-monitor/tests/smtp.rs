//! The email channel (`smtp` feature) against a local SMTP stub: STARTTLS
//! and implicit TLS, credentials from a secret, redaction, and the refusal
//! of a server that offers no STARTTLS. Run with
//! `cargo test -p rustman-monitor --features smtp --test smtp`.
#![cfg(feature = "smtp")]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use rustman_core::Environment;
use rustman_core::model::{VarValue, Variables};
use rustman_core::settings::Settings;
use rustman_core::vars::SecretLookup;
use rustman_http::HttpEngine;
use rustman_monitor::alerts::{Channel, EmailChannel, EmailTls};
use rustman_monitor::notify::{Alert, AlertKind, Desktop, Notifier};
use rustman_test_support::TestCerts;
use rustman_test_support::server::{Tls, server_config};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

const PASSWORD: &str = "canary-Smtp-passw0rd";
const TOKEN: &str = "canary-T0ken-in-message";

/// What the stub received.
#[derive(Clone, Debug, Default)]
struct Mail {
    auth: Option<String>,
    from: String,
    to: Vec<String>,
    data: String,
    plain_commands: Vec<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Starttls,
    Implicit,
    /// Never offers STARTTLS.
    PlainOnly,
}

struct Stub {
    port: u16,
    mail: Arc<Mutex<Mail>>,
}

impl Stub {
    async fn start(certs: &TestCerts, mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut config = server_config(certs, Tls::Http1Only);
        config.alpn_protocols.clear();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let mail = Arc::new(Mutex::new(Mail::default()));
        let shared = mail.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let (acceptor, mail) = (acceptor.clone(), shared.clone());
                tokio::spawn(async move { session(tcp, acceptor, mode, mail).await });
            }
        });
        Self { port, mail }
    }

    fn mail(&self) -> Mail {
        self.mail.lock().unwrap().clone()
    }
}

async fn session(tcp: TcpStream, acceptor: TlsAcceptor, mode: Mode, mail: Arc<Mutex<Mail>>) {
    if mode == Mode::Implicit {
        let tls = acceptor.accept(tcp).await.unwrap();
        converse(BufReader::new(tls), true, mail).await;
        return;
    }
    let mut plain = BufReader::new(tcp);
    let upgrade = converse_plain(&mut plain, mode, &mail).await;
    if upgrade {
        let tls = acceptor.accept(plain.into_inner()).await.unwrap();
        converse(BufReader::new(tls), false, mail).await;
    }
}

/// The plain part: greeting, EHLO, then STARTTLS (true) or the client
/// giving up (false).
async fn converse_plain(
    io: &mut BufReader<TcpStream>,
    mode: Mode,
    mail: &Arc<Mutex<Mail>>,
) -> bool {
    io.write_all(b"220 stub ESMTP\r\n").await.unwrap();
    let mut line = String::new();
    loop {
        line.clear();
        if io.read_line(&mut line).await.unwrap_or(0) == 0 {
            return false;
        }
        let command = line.trim_end().to_owned();
        mail.lock().unwrap().plain_commands.push(command.clone());
        let upper = command.to_ascii_uppercase();
        let reply: &[u8] = if upper.starts_with("EHLO") && mode == Mode::Starttls {
            b"250-stub\r\n250 STARTTLS\r\n"
        } else if upper.starts_with("EHLO") {
            b"250-stub\r\n250 AUTH PLAIN\r\n"
        } else if upper == "STARTTLS" {
            io.write_all(b"220 go ahead\r\n").await.unwrap();
            return true;
        } else if upper == "QUIT" {
            let _ = io.write_all(b"221 bye\r\n").await;
            return false;
        } else {
            b"530 must issue STARTTLS first\r\n"
        };
        io.write_all(reply).await.unwrap();
    }
}

/// The TLS part: EHLO, AUTH PLAIN, MAIL, RCPT, DATA, QUIT.
async fn converse<S: AsyncRead + AsyncWrite + Unpin>(
    mut io: BufReader<S>,
    greet: bool,
    mail: Arc<Mutex<Mail>>,
) {
    if greet {
        io.write_all(b"220 stub ESMTP\r\n").await.unwrap();
    }
    let mut line = String::new();
    loop {
        line.clear();
        if io.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let command = line.trim_end().to_owned();
        let upper = command.to_ascii_uppercase();
        let reply: &[u8] = if upper.starts_with("EHLO") {
            b"250-stub\r\n250 AUTH PLAIN LOGIN\r\n"
        } else if let Some(token) = command.strip_prefix("AUTH PLAIN ") {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(token.trim())
                .unwrap();
            mail.lock().unwrap().auth = Some(String::from_utf8_lossy(&decoded).into_owned());
            b"235 ok\r\n"
        } else if upper.starts_with("MAIL FROM:") {
            mail.lock().unwrap().from = command[10..].to_owned();
            b"250 ok\r\n"
        } else if upper.starts_with("RCPT TO:") {
            mail.lock().unwrap().to.push(command[8..].to_owned());
            b"250 ok\r\n"
        } else if upper == "DATA" {
            io.write_all(b"354 go\r\n").await.unwrap();
            let data = read_data(&mut io).await;
            mail.lock().unwrap().data = data;
            b"250 queued\r\n"
        } else if upper == "QUIT" {
            let _ = io.write_all(b"221 bye\r\n").await;
            return;
        } else {
            b"502 unknown\r\n"
        };
        io.write_all(reply).await.unwrap();
    }
}

async fn read_data<S: AsyncRead + AsyncWrite + Unpin>(io: &mut BufReader<S>) -> String {
    let mut data = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        io.read_line(&mut line).await.unwrap();
        if line == ".\r\n" || line.is_empty() {
            return data;
        }
        data.push_str(&line);
    }
}

struct Secrets;

impl SecretLookup for Secrets {
    fn secret(&self, _env: &str, name: &str) -> Result<Option<String>, String> {
        Ok(match name {
            "smtp_password" => Some(PASSWORD.to_owned()),
            "token" => Some(TOKEN.to_owned()),
            _ => None,
        })
    }
}

struct NoDesktop;

impl Desktop for NoDesktop {
    fn show(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
}

/// A workspace whose `[http] ca_bundle` trusts the test CA.
fn workspace(certs: &TestCerts) -> (tempfile::TempDir, Settings) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ca.pem"), &certs.ca_pem).unwrap();
    let text = "[workspace]\nname = \"t\"\nversion = 1\n[http]\nca_bundle = \"ca.pem\"\n";
    let (settings, _) = Settings::parse(Path::new("rustman.toml"), text).unwrap();
    (dir, settings)
}

fn environment() -> Environment {
    Environment {
        name: "prod".into(),
        variables: Variables::new(vec![(
            "smtp_user".into(),
            VarValue::String("monitor".into()),
        )]),
        secrets: vec!["smtp_password".into(), "token".into()],
        ..Environment::default()
    }
}

fn channel(port: u16, tls: EmailTls) -> Channel {
    Channel::Email(EmailChannel {
        host: "127.0.0.1".into(),
        port,
        from: "monitor@example.com".into(),
        to: vec!["ops@example.com".into()],
        username: Some("{{smtp_user}}".into()),
        password: Some("{{smtp_password}}".into()),
        tls,
    })
}

fn alert() -> Alert {
    Alert {
        monitor: "api-health".into(),
        environment: "prod".into(),
        kind: AlertKind::Firing,
        rule: Some("consecutive_failures: 2".into()),
        at: "2026-03-01T10:00:00Z".parse().unwrap(),
        failure: Some(format!("health/status: token {TOKEN} rejected")),
        duration: None,
    }
}

async fn send(certs: &TestCerts, channel: Channel) -> Option<String> {
    let (dir, settings) = workspace(certs);
    let env = environment();
    let engine = HttpEngine::new();
    let notifier = Notifier {
        root: dir.path(),
        settings: &settings,
        environment: &env,
        secrets: &Secrets,
        desktop: &NoDesktop,
        engine: &engine,
        timeout: Duration::from_secs(10),
    };
    let deliveries = notifier.send(&[channel], &alert()).await;
    assert_eq!(deliveries[0].channel, "email (127.0.0.1)");
    deliveries[0].error.clone()
}

fn check_delivered(mail: &Mail) {
    assert_eq!(
        mail.auth.as_deref(),
        Some(format!("\0monitor\0{PASSWORD}").as_str())
    );
    assert!(mail.from.contains("monitor@example.com"), "{mail:?}");
    assert!(mail.to[0].contains("ops@example.com"), "{mail:?}");
    assert!(
        mail.data
            .contains("Subject: [FIRING] api-health: consecutive_failures: 2"),
        "{}",
        mail.data
    );
    assert!(!mail.data.contains(TOKEN), "{}", mail.data);
    assert!(mail.data.contains("redacted:token"), "{}", mail.data);
}

#[tokio::test]
async fn starttls_delivers_with_credentials_and_redacts_the_body() {
    let certs = TestCerts::generate();
    let stub = Stub::start(&certs, Mode::Starttls).await;
    let error = send(&certs, channel(stub.port, EmailTls::Starttls)).await;
    assert_eq!(error, None);
    let mail = stub.mail();
    check_delivered(&mail);
    // Nothing but EHLO and STARTTLS went over the plain connection.
    assert!(
        mail.plain_commands
            .iter()
            .all(|c| c.starts_with("EHLO") || c == "STARTTLS"),
        "{:?}",
        mail.plain_commands
    );
}

#[tokio::test]
async fn implicit_tls_delivers() {
    let certs = TestCerts::generate();
    let stub = Stub::start(&certs, Mode::Implicit).await;
    let error = send(&certs, channel(stub.port, EmailTls::Tls)).await;
    assert_eq!(error, None);
    check_delivered(&stub.mail());
}

/// Plain-text SMTP is refused: without STARTTLS nothing is sent, and the
/// error never shows the password.
#[tokio::test]
async fn a_server_without_starttls_is_refused() {
    let certs = TestCerts::generate();
    let stub = Stub::start(&certs, Mode::PlainOnly).await;
    let error = send(&certs, channel(stub.port, EmailTls::Starttls))
        .await
        .expect("refused");
    assert!(!error.contains(PASSWORD), "{error}");
    let mail = stub.mail();
    assert!(mail.auth.is_none() && mail.data.is_empty(), "{mail:?}");
    assert!(
        !mail
            .plain_commands
            .iter()
            .any(|c| c.starts_with("AUTH") || c.starts_with("MAIL")),
        "{:?}",
        mail.plain_commands
    );
}
