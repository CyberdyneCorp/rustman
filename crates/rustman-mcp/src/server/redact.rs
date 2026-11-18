//! The second redaction pass (spec "Secret redaction in results", design
//! D6). Engine output is already redacted; this pass runs again over the
//! final JSON of every tool result, resource, log and progress message with
//! the session's secret values (and their base64 forms) and the sensitive
//! header names, so no code path can skip it.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use rustman_core::model::{Auth, NameValue, Request};
use rustman_core::redact::{MIN_HEADER_CREDENTIAL, remember_for_logs};
use rustman_core::secret::placeholder;
use rustman_core::{Redactor, Secret, Workspace};
use serde_json::Value;

use super::error::ToolError;

/// Headers whose values agents never see, in their canonical spelling: the
/// engine's sensitive headers plus `X-Api-Key`.
pub const AGENT_SENSITIVE_HEADERS: [&str; 5] = [
    "Authorization",
    "Proxy-Authorization",
    "Cookie",
    "Set-Cookie",
    "X-Api-Key",
];

/// The canonical name of `name` when agents never see its value.
pub fn sensitive_header(name: &str) -> Option<&'static str> {
    AGENT_SENSITIVE_HEADERS
        .into_iter()
        .find(|h| h.eq_ignore_ascii_case(name.trim()))
}

/// The redactor of everything a session sends to the client.
#[derive(Clone, Debug, Default)]
pub struct OutputRedactor {
    redactor: Redactor,
}

impl OutputRedactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Redact `secret`'s value and its base64 forms (standard and URL-safe,
    /// padded or not) under its name. Stderr logs redact them too.
    pub fn add_secret(&mut self, secret: &Secret<String>) {
        let value = secret.expose();
        if value.is_empty() {
            return;
        }
        self.redactor.add(secret);
        for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
            self.redactor
                .add_value(secret.name(), &engine.encode(value));
        }
        remember_for_logs(&self.redactor);
    }

    /// Also redact what `other` knows (an exchange's credentials).
    pub fn merge(&mut self, other: &Redactor) {
        self.redactor.merge(other);
        remember_for_logs(&self.redactor);
    }

    /// This pass, also redacting the literal credentials `request` holds.
    pub fn with_request_literals(&self, request: &Request) -> Self {
        let mut out = self.clone();
        out.add_request_literals(request);
        out
    }

    /// Redact the literal credentials of every request and collection of
    /// `workspace`: a value pasted into a tracked file is never a secret
    /// of the store, so no other pass knows it.
    pub fn add_workspace_literals(&mut self, workspace: &Workspace) {
        let collections = std::iter::once(&workspace.tree).chain(workspace.collections());
        for collection in collections {
            let meta = &collection.meta;
            self.add_literals(meta.headers.as_deref(), meta.auth.as_ref());
        }
        for request in workspace.requests() {
            self.add_request_literals(request.request());
        }
        remember_for_logs(&self.redactor);
    }

    /// The literal credentials of a request: its sensitive header values
    /// and its auth block. Saved files are returned as text, where
    /// [`mask_headers`] cannot see a header.
    pub fn add_request_literals(&mut self, request: &Request) {
        if let Request::Http(http) = request {
            self.add_literals(http.headers.as_deref(), http.auth.as_ref());
        }
    }

    fn add_literals(&mut self, headers: Option<&[NameValue]>, auth: Option<&Auth>) {
        for header in headers.into_iter().flatten() {
            self.add_header_literal(&header.name, &header.value);
        }
        if let Some(auth) = auth {
            self.add_auth_literal(auth);
        }
    }

    /// The literal value of a sensitive header: its credentials (after the
    /// scheme, each cookie value) and the whole value. Values with a
    /// `{{name}}` reference are left to the secret pass.
    fn add_header_literal(&mut self, header: &str, value: &str) {
        let Some(name) = sensitive_header(header) else {
            return;
        };
        let value = value.trim();
        if value.contains("{{") {
            return;
        }
        self.redactor.add_header_value(name, value);
        let credential = value.split_once(' ').map_or(value, |(_, c)| c.trim());
        self.add_literal(name, value);
        self.add_literal(name, credential);
    }

    /// The literal credentials of an auth block, as written in the file and
    /// as the request carries them.
    fn add_auth_literal(&mut self, auth: &Auth) {
        self.redactor.add_auth(auth);
        match auth {
            Auth::Basic { password, .. } => self.add_literal("password", password),
            Auth::Bearer { token } => self.add_literal("Authorization", token),
            Auth::ApiKey { name, value, .. } => {
                let name = sensitive_header(name).unwrap_or("api_key");
                self.add_literal(name, value);
            }
            _ => {}
        }
    }

    /// Redact `value` when it is a literal long enough to tell.
    fn add_literal(&mut self, name: &str, value: &str) {
        let value = value.trim();
        if !value.contains("{{") && value.chars().count() >= MIN_HEADER_CREDENTIAL {
            self.redactor.add_value(name, value);
        }
    }

    /// The engine redactor behind this pass.
    pub fn redactor(&self) -> &Redactor {
        &self.redactor
    }

    /// `text` with every known secret replaced: for log and progress
    /// messages.
    pub fn text(&self, text: &str) -> String {
        self.redactor.text(text).into_owned()
    }

    /// `value` with sensitive header values masked, then every known secret
    /// replaced in its serialized form. Fails closed when the redacted text
    /// is no longer JSON.
    pub fn json(&self, value: &Value) -> Result<Value, ToolError> {
        let mut value = value.clone();
        mask_headers(&mut value);
        let text = serde_json::to_string(&value)
            .map_err(|e| ToolError::internal(format!("cannot serialize the result: {e}")))?;
        let redacted = self.redactor.text(&text);
        serde_json::from_str(&redacted)
            .map_err(|_| ToolError::internal("the result could not be redacted safely"))
    }
}

/// Mask the values of sensitive headers wherever `value` holds a header: as
/// an object key, as a `{"name", "value"}` object or as a `[name, value]`
/// pair.
pub fn mask_headers(value: &mut Value) {
    match value {
        Value::Object(map) => {
            mask_named_value(map);
            for (key, field) in map.iter_mut() {
                match sensitive_header(key) {
                    Some(name) if !field.is_object() => *field = masked(name),
                    _ => mask_headers(field),
                }
            }
        }
        Value::Array(items) => {
            mask_pair(items);
            items.iter_mut().for_each(mask_headers);
        }
        _ => {}
    }
}

fn masked(name: &str) -> Value {
    Value::String(placeholder(name))
}

/// `{"name": "Authorization", "value": "..."}`.
fn mask_named_value(map: &mut serde_json::Map<String, Value>) {
    let name = map
        .get("name")
        .and_then(Value::as_str)
        .and_then(sensitive_header);
    if let (Some(name), Some(value)) = (name, map.get_mut("value")) {
        *value = masked(name);
    }
}

/// `["Authorization", "..."]`.
fn mask_pair(items: &mut [Value]) {
    if let [Value::String(name), value @ Value::String(_)] = items
        && let Some(name) = sensitive_header(name)
    {
        *value = masked(name);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const TOKEN: &str = "tok-3f9a1c77e2b84d0c";

    fn redactor() -> OutputRedactor {
        let mut out = OutputRedactor::new();
        out.add_secret(&Secret::new("api_token", TOKEN.to_owned()));
        out
    }

    #[test]
    fn secret_values_and_their_encoded_forms_are_replaced() {
        let out = redactor();
        let encoded = STANDARD.encode(TOKEN);
        let url_safe = URL_SAFE_NO_PAD.encode(TOKEN);
        let value = json!({
            "body": format!("{{\"token\":\"{TOKEN}\"}}"),
            "b64": encoded,
            "url": format!("http://x/?t={url_safe}&q=1"),
            "percent": "tok%2D3f9a1c77e2b84d0c",
        });
        let redacted = out.json(&value).unwrap();
        let text = redacted.to_string();
        assert!(!text.contains(TOKEN), "{text}");
        assert!(!text.contains(&encoded), "{text}");
        assert!(!text.contains(&url_safe), "{text}");
        assert_eq!(redacted["b64"], "«redacted:api_token»");
        assert!(text.contains("«redacted:api_token»"));
    }

    #[test]
    fn sensitive_headers_are_masked_in_every_shape() {
        let value = json!({
            "headers": {"authorization": "Bearer abc", "X-API-KEY": "k-123", "Accept": "*/*"},
            "list": [["Set-Cookie", "sid=1"], ["Content-Type", "text/plain"]],
            "named": [{"name": "Cookie", "value": "a=b"}, {"name": "Host", "value": "x"}],
        });
        let redacted = OutputRedactor::new().json(&value).unwrap();
        assert_eq!(
            redacted["headers"]["authorization"],
            "«redacted:Authorization»"
        );
        assert_eq!(redacted["headers"]["X-API-KEY"], "«redacted:X-Api-Key»");
        assert_eq!(redacted["headers"]["Accept"], "*/*");
        assert_eq!(redacted["list"][0][1], "«redacted:Set-Cookie»");
        assert_eq!(redacted["list"][1][1], "text/plain");
        assert_eq!(redacted["named"][0]["value"], "«redacted:Cookie»");
        assert_eq!(redacted["named"][1]["value"], "x");
    }

    #[test]
    fn literal_header_values_of_a_saved_request_are_redacted() {
        let yaml = "version: 1\nkind: http\nname: lit\nmethod: GET\nurl: http://x/\nheaders:\n  - name: Authorization\n    value: \"Bearer literal-hardcoded-abc123\"\n  - name: x-api-key\n    value: litkey-999\n  - name: Accept\n    value: application/json\n  - name: Cookie\n    value: \"sid={{session}}\"\nauth:\n  type: basic\n  username: admin\n  password: hunter2-pass\n";
        let request = rustman_core::model::KindRegistry::default()
            .load(std::path::Path::new("lit.rman.yaml"), yaml)
            .unwrap();
        let out = OutputRedactor::new().with_request_literals(&request);
        let text = out.text(yaml);
        for leaked in ["literal-hardcoded-abc123", "litkey-999", "hunter2-pass"] {
            assert!(!text.contains(leaked), "{leaked} in {text}");
        }
        assert!(text.contains("«redacted:Authorization»"), "{text}");
        assert!(text.contains("«redacted:X-Api-Key»"), "{text}");
        assert!(text.contains("application/json"), "{text}");
        assert!(text.contains("sid={{session}}"), "{text}");
        assert!(OutputRedactor::new().text(yaml).contains("litkey-999"));
    }

    #[test]
    fn log_and_progress_text_is_redacted() {
        let out = redactor();
        let line = format!("sent GET http://x/?token={TOKEN}");
        assert_eq!(
            out.text(&line),
            "sent GET http://x/?token=«redacted:api_token»"
        );
        assert!(!rustman_core::redact::redact_logs(&line).contains(TOKEN));
    }
}
