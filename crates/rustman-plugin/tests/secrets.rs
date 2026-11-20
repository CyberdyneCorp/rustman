//! Secrets inside plugins (plugins spec, "Secrets inside plugins"; task
//! 3.4): a value is revealed only with `secrets:read:<name>`, values handed
//! to a plugin without it carry the placeholder, and whatever a plugin
//! logs is redacted.

mod common;

use common::{plugin, verified};
use rustman_core::model::RequestSettings;
use rustman_core::run::SentRequest;
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedPair, ResolvedRequest, TlsFiles};
use rustman_core::{Redactor, Secret};
use rustman_plugin::fixtures;
use rustman_plugin::plugin::{AuthRequest, Header, Message};
use rustman_plugin::sandbox::{HostContext, LogLevel, PluginLog};

const AWS_SECRET: &str = "wJalrXUtnFEMI-K7MDENG-bPxRfiCYEXAMPLEKEY";
const API_TOKEN: &str = "tok-5f1c2a9e7b3d";

fn context() -> HostContext {
    HostContext::new(".").with_secrets(vec![
        Secret::new("aws_secret_key", AWS_SECRET.to_owned()),
        Secret::new("api_token", API_TOKEN.to_owned()),
    ])
}

fn auth_request() -> AuthRequest {
    AuthRequest {
        scheme: "v1".into(),
        method: "GET".into(),
        url: "https://api.example.com/orders".into(),
        headers: Vec::new(),
        body: Vec::new(),
        params: Vec::new(),
    }
}

#[test]
fn auth_plugin_with_a_grant() {
    let manifest = fixtures::manifest("aws-sigv4", &["auth"], &["secrets:read", "clock"]);
    let grants = ["secrets:read:aws_secret_key", "clock"];
    let verified = verified("aws-sigv4", fixtures::signer(), &manifest, &grants);
    let plugin = plugin(&verified, context());
    assert_eq!(plugin.auth_schemes().unwrap(), ["v1"]);

    let output = plugin.sign(&auth_request()).unwrap();

    // The plugin received the real value...
    assert_eq!(
        output.set_headers,
        [Header {
            name: "Authorization".into(),
            value: AWS_SECRET.into(),
        }]
    );
    // ...and a recorded request carrying it would show it redacted (real
    // runs do not record auth headers; see the CLI canary tests).
    let sent = sent_with_header("Authorization", AWS_SECRET);
    let recorded = serde_json::to_string(&sent).unwrap();
    assert!(recorded.contains("«redacted:Authorization»"), "{recorded}");
    assert!(!recorded.contains(AWS_SECRET), "{recorded}");
}

#[test]
fn auth_plugin_without_the_grant_cannot_reveal() {
    let manifest = fixtures::manifest("aws-sigv4", &["auth"], &["secrets:read"]);
    let verified = verified("aws-sigv4", fixtures::signer(), &manifest, &[]);
    let plugin = plugin(&verified, context());
    let err = plugin.sign(&auth_request()).unwrap_err();
    assert_eq!(err.to_string(), "plugin aws-sigv4: secrets aws_secret_key");
    assert_eq!(
        plugin.take_denials(),
        ["capability_denied: secrets aws_secret_key"]
    );
}

#[test]
fn plugin_without_a_grant_sees_the_placeholder() {
    let manifest = fixtures::manifest("acme", &["transformer"], &[]);
    let verified = verified("acme", fixtures::echo_transformer(), &manifest, &[]);
    let plugin = plugin(&verified, context());
    let body = format!("{{\"token\":\"{API_TOKEN}\"}}");
    let redactor = plugin.input_redactor();
    let message = Message {
        headers: vec![Header {
            name: "X-Token".into(),
            value: redactor.text(API_TOKEN).into_owned(),
        }],
        body: redactor.bytes(body.as_bytes()).into_owned(),
    };

    let seen = plugin
        .transform_request("echo", "https://api.example.com/", &message)
        .unwrap();

    let seen_body = String::from_utf8(seen.body).unwrap();
    assert_eq!(seen_body, "{\"token\":\"«redacted:api_token»\"}");
    assert_eq!(seen.headers[0].value, "«redacted:api_token»");
}

#[test]
fn a_granted_secret_reaches_the_plugin_in_clear() {
    let manifest = fixtures::manifest("acme", &["transformer"], &["secrets:read"]);
    let verified = verified(
        "acme",
        fixtures::echo_transformer(),
        &manifest,
        &["secrets:read:api_token"],
    );
    let plugin = plugin(&verified, context());
    let redactor = plugin.input_redactor();
    assert_eq!(redactor.text(API_TOKEN), API_TOKEN);
    assert_eq!(redactor.text(AWS_SECRET), "«redacted:aws_secret_key»");
}

#[test]
fn plugin_echoes_a_secret() {
    let manifest = fixtures::manifest("echo", &["generator"], &["secrets:read"]);
    let verified = verified(
        "echo",
        fixtures::secret_echo(),
        &manifest,
        &["secrets:read:api_token"],
    );
    let plugin = plugin(&verified, context());

    let value = plugin.generate("echo", &[]).unwrap();

    assert_eq!(value, API_TOKEN);
    assert_eq!(
        plugin.take_logs(),
        [PluginLog {
            level: LogLevel::Info,
            message: "«redacted:api_token»".into(),
        }]
    );
}

#[test]
fn plugin_errors_are_redacted() {
    let manifest = fixtures::manifest("echo", &["generator"], &["secrets:read"]);
    let verified = verified("echo", fixtures::secret_echo(), &manifest, &[]);
    let context = HostContext::new(".").with_secrets(vec![Secret::new(
        "api_token",
        // A secret whose value is the denial text itself: an error that
        // echoes it must still be redacted.
        "secrets api_token".to_owned(),
    )]);
    let plugin = plugin(&verified, context);
    let err = plugin.generate("echo", &[]).unwrap_err();
    assert_eq!(err.to_string(), "plugin echo: «redacted:api_token»");
}

fn sent_with_header(name: &str, value: &str) -> SentRequest {
    let request = ResolvedRequest {
        method: "GET".into(),
        url: Resolved::literal("https://api.example.com/orders"),
        query: Vec::new(),
        headers: vec![ResolvedPair {
            name: Resolved::literal(name),
            value: Resolved::literal(value),
        }],
        body: ResolvedBody::None,
        auth: None,
        settings: RequestSettings::default(),
        tls: TlsFiles::default(),
        secrets: Vec::new(),
    };
    SentRequest::new(&request, &Redactor::for_request(&request))
}
