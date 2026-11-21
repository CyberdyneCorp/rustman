//! Transformers, request kinds and importers served by a loaded plugin set
//! (add-plugin-system tasks 5.4 to 5.6), called through [`PluginRuntime`]
//! as the runner and `rustman import` call them.

mod common;

use common::{Ws, verified, verified_with};
use rustman_core::Secret;
use rustman_core::plugins::{
    Extensions, FieldEditor, PaneField, PaneView, PluginMessage, PluginPane,
};
use rustman_plugin::fixtures;
use rustman_plugin::limits::Limits;
use rustman_plugin::sandbox::HostContext;
use rustman_plugin::workspace::PluginSet;
use rustman_plugin::{LoadError, PluginRuntime};
use serde_json::json;

const API_TOKEN: &str = "tok-7d2e91f0c4";

/// A runtime over `ws` with one plugin, `name`, holding `grants`.
fn runtime(
    ws: &Ws,
    name: &str,
    wasm: Vec<u8>,
    point: &str,
    caps: &[&str],
    grants: &[&str],
) -> Result<PluginRuntime, LoadError> {
    let manifest = fixtures::manifest(name, &[point], caps);
    let set = PluginSet {
        plugins: vec![verified(name, wasm, &manifest, grants)],
        ..PluginSet::default()
    };
    let context = HostContext::new(ws.root())
        .with_secrets(vec![Secret::new("api_token", API_TOKEN.to_owned())]);
    PluginRuntime::new(&set, context)
}

fn xor(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|b| b ^ 1).collect()
}

/// "Decrypt a response before assertions", at the runtime: the response
/// transformer returns the decrypted body and keeps the headers.
#[test]
fn response_transformers_decrypt() {
    let ws = Ws::new();
    let acme = runtime(
        &ws,
        "acme",
        fixtures::xor_transformer(),
        "transformer",
        &[],
        &[],
    )
    .unwrap();
    let message = PluginMessage {
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: xor(br#"{"total":42}"#),
    };
    let decrypted = acme
        .transform_response("acme/decrypt", 200, &message)
        .unwrap();
    assert_eq!(decrypted.body, br#"{"total":42}"#);
    assert_eq!(decrypted.headers, message.headers);
}

/// "Plugin without a grant": a request transformer without secret grants
/// sees the placeholder in place of the secret.
#[test]
fn transformers_without_a_grant_see_placeholders() {
    let ws = Ws::new();
    let acme = runtime(
        &ws,
        "acme",
        fixtures::xor_transformer(),
        "transformer",
        &[],
        &[],
    )
    .unwrap();
    let message = PluginMessage {
        headers: vec![("X-Token".into(), API_TOKEN.into())],
        body: format!("token={API_TOKEN}").into_bytes(),
    };
    let encrypted = acme
        .transform_request("acme/encrypt", "https://api.example.com/", &message)
        .unwrap();
    let seen = String::from_utf8(xor(&encrypted.body)).unwrap();
    assert_eq!(seen, "token=«redacted:api_token»");
    assert_eq!(encrypted.headers[0].1, "«redacted:api_token»");
}

#[test]
fn unknown_transformers_fail() {
    let ws = Ws::new();
    let acme = runtime(
        &ws,
        "acme",
        fixtures::xor_transformer(),
        "transformer",
        &[],
        &[],
    )
    .unwrap();
    let message = PluginMessage::default();
    let error = acme
        .transform_response("other/decrypt", 200, &message)
        .unwrap_err();
    assert_eq!(error, "plugin other is not loaded");
}

/// "Plugin kind loads", at the runtime: the kind, its pane and its schema
/// are registered, and a request runs through the plugin.
#[test]
fn plugin_kinds_register_validate_and_execute() {
    let ws = Ws::new();
    let soap = runtime(
        &ws,
        "soapy",
        fixtures::kind_plugin("soap"),
        "kind",
        &[],
        &[],
    )
    .unwrap();
    let kinds = soap.kinds();
    assert_eq!(kinds.len(), 1);
    assert_eq!(kinds[0].name, "soap");
    assert_eq!(kinds[0].plugin, "soapy");
    let field = |name: &str, label: &str, editor| PaneField {
        name: name.into(),
        label: label.into(),
        editor,
    };
    let options = vec!["GetOrder".to_owned(), "ListOrders".to_owned()];
    assert_eq!(
        kinds[0].pane,
        PluginPane {
            fields: vec![
                field("endpoint", "Endpoint", FieldEditor::Text),
                field("action", "SOAP action", FieldEditor::Select(options)),
                field("envelope", "Envelope", FieldEditor::Code("xml".into())),
            ],
            views: vec![PaneView::Raw, PaneView::JsonTree],
        }
    );

    let fields = json!({"endpoint": "https://api.example.com/soap", "action": "GetOrder"});
    soap.validate_kind("soap", &fields).unwrap();
    let error = soap
        .validate_kind("soap", &json!({"endpoint": 1}))
        .unwrap_err();
    assert!(error.starts_with("plugin soapy: kind 'soap': "), "{error}");
    assert!(error.contains("action"), "{error}");
    // As docs/plugins.md quotes it.
    let error = soap
        .validate_kind("soap", &json!({"endpoint": "https://api.example.com/soap"}))
        .unwrap_err();
    assert_eq!(
        error,
        r#"plugin soapy: kind 'soap': /: "action" is a required property"#
    );

    let outcome = soap.execute_kind("soap", &fields).unwrap();
    assert_eq!(outcome.status, Some(200));
    assert_eq!(outcome.summary, format!("soap {fields}"));
    assert_eq!(outcome.body, fields.to_string().into_bytes());
    let error = soap.execute_kind("rest", &fields).unwrap_err();
    assert_eq!(error, "unknown request kind 'rest'");
}

/// "Built-in kind collision".
#[test]
fn builtin_kinds_are_reserved() {
    let ws = Ws::new();
    let error = runtime(
        &ws,
        "sneaky",
        fixtures::kind_plugin("http"),
        "kind",
        &[],
        &[],
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "plugin sneaky: kind 'http' is reserved");
}

/// The messaging kinds are built in too: a plugin cannot take them over.
#[test]
fn messaging_kinds_are_reserved() {
    let ws = Ws::new();
    for kind in ["kafka", "mqtt", "amqp"] {
        let error =
            runtime(&ws, "sneaky", fixtures::kind_plugin(kind), "kind", &[], &[]).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("plugin sneaky: kind '{kind}' is reserved")
        );
    }
}

/// An importer reads its source only through an `fs:read` grant, and what
/// it returns is redacted of every secret of the session.
#[test]
fn importers_read_granted_sources_only() {
    let ws = Ws::new();
    ws.write("src/a.txt", format!("token: {API_TOKEN}\n"));
    ws.write("other/b.txt", "b\n");
    let caps = ["fs:read"];
    let copy = runtime(
        &ws,
        "copier",
        fixtures::copy_importer(),
        "importer",
        &caps,
        &["fs:read:src"],
    )
    .unwrap();
    assert_eq!(copy.import_formats(), ["copy"]);
    let files = copy.import("copy", "src/a.txt").unwrap();
    assert_eq!(files.plugin, "copier");
    assert_eq!(
        files.requests,
        [(
            "imported.rman.yaml".to_owned(),
            "token: «redacted:api_token»\n".to_owned()
        )]
    );
    assert!(files.environments.is_empty());

    let denied = copy.import("copy", "other/b.txt").unwrap_err();
    assert_eq!(denied, "plugin copier: capability_denied: fs other/b.txt");
    let escape = copy.import("copy", "src/../../x").unwrap_err();
    assert!(escape.contains("capability_denied: fs"), "{escape}");
    let unknown = copy.import("soapui", "src/a.txt").unwrap_err();
    assert_eq!(unknown, "unknown import format 'soapui'");

    let ungranted = runtime(
        &ws,
        "copier",
        fixtures::copy_importer(),
        "importer",
        &caps,
        &[],
    )
    .unwrap();
    let denied = ungranted.import("copy", "src/a.txt").unwrap_err();
    assert_eq!(denied, "plugin copier: capability_denied: fs src/a.txt");
}

/// An import source larger than the importer's memory limit is refused
/// before it is read whole into the host's memory.
#[test]
fn import_sources_are_capped_at_the_memory_limit() {
    let ws = Ws::new();
    ws.write("src/big.txt", "x".repeat(1024 * 1024 + 1));
    let manifest = fixtures::manifest("copier", &["importer"], &["fs:read"]);
    let limits = Limits {
        memory_mib: 1,
        ..Limits::default()
    };
    let wasm = fixtures::copy_importer();
    let set = PluginSet {
        plugins: vec![verified_with(
            "copier",
            wasm,
            &manifest,
            &["fs:read:src"],
            limits,
        )],
        ..PluginSet::default()
    };
    let copy = PluginRuntime::new(&set, HostContext::new(ws.root())).unwrap();
    let refused = copy.import("copy", "src/big.txt").unwrap_err();
    assert_eq!(
        refused,
        "plugin copier: cannot read src/big.txt: file larger than 1048576 bytes"
    );
}
