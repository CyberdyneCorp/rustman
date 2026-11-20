//! The send gate of a stress VU: the bench gate (the safety policy, the
//! `[stress]` governor), plus fuzz mixing. Before a fuzzed slot's request
//! is sent, the VU arms the gate with the slot; the gate then sends the
//! slot's variant as a body override of the resolved request, so auth,
//! cookies, headers and timing are those of the request itself, and
//! remembers what it sent for the findings. A slot whose request has no
//! JSON body to mutate is sent as is, and counts as valid traffic.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bytes::Bytes;
use rustman_core::Redactor;
use rustman_core::vars::{ResolvedBody, ResolvedRequest};
use rustman_explore::mutate;
use rustman_http::runner::SendGate;
use rustman_http::{BodyOverride, Exchange, RequestError, SendOptions};
use serde_json::Value;
use tokio::sync::OnceCell;

use crate::safety::BenchGate;
use crate::stress::fuzz::{FuzzPlan, Source};
use crate::stress::malformed::{self, Payload};

/// The longest body preview kept as evidence.
pub const PREVIEW: usize = 256;

/// What a fuzzed slot sent in place of its request's body.
#[derive(Clone, Debug, PartialEq)]
pub struct Fuzzed {
    /// `mutation`, `schema`, or the malformed family's name.
    pub family: String,
    /// What was changed, such as `type-flip at /qty` or `depth 5000`.
    pub label: String,
    /// What grows in the family, and how much (`depth`, `bytes`, `keys`).
    pub measure: Option<(&'static str, u64)>,
    /// The body's size once decoded.
    pub expanded: u64,
    /// The input breaks the request's contract: a 2xx accepts it.
    pub invalid: bool,
    /// Over a documented `maxLength` or `maxItems`.
    pub oversized: bool,
    /// The body as sent (unredacted: shown only through [`Fuzzed::preview`]).
    pub body: Bytes,
    /// The body sent, for replays (unredacted: never shown).
    pub input: Option<BodyOverride>,
}

/// Lazily built compression bomb, shared by every VU of a run.
pub(crate) type BombCell = Arc<OnceCell<Option<Payload>>>;

/// A VU's gate.
pub(crate) struct StressGate {
    inner: BenchGate,
    plan: Option<Arc<FuzzPlan>>,
    bomb: BombCell,
    armed: Mutex<Option<u64>>,
    sent: Mutex<Option<Fuzzed>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl StressGate {
    pub fn new(inner: BenchGate, plan: Option<Arc<FuzzPlan>>, bomb: BombCell) -> Self {
        Self {
            inner,
            plan,
            bomb,
            armed: Mutex::new(None),
            sent: Mutex::new(None),
        }
    }

    /// Fuzz the next request sent, as slot `slot`.
    pub fn arm(&self, slot: u64) {
        *lock(&self.armed) = Some(slot);
        *lock(&self.sent) = None;
    }

    /// What the last armed send replaced its body with; `None` when it was
    /// sent as is.
    pub fn take_fuzzed(&self) -> Option<Fuzzed> {
        lock(&self.armed).take();
        lock(&self.sent).take()
    }

    async fn variant(
        &self,
        request: &ResolvedRequest,
        slot: u64,
    ) -> Option<(BodyOverride, Fuzzed)> {
        let plan = self.plan.as_deref()?;
        let base = json_body(&request.body);
        let pick = plan.variant_pick(slot);
        match plan.source(slot)? {
            Source::Mutation => mutation(plan, request, base.as_ref()?, pick),
            Source::Malformed(family) => {
                let payload = if family == malformed::Family::CompressionBomb {
                    self.bomb
                        .get_or_init(|| async { malformed::bomb(&plan.caps).await.ok() })
                        .await
                        .clone()?
                } else {
                    plan.payloads
                        .generate(family, base.as_ref(), pick, &plan.caps)
                };
                Some(from_payload(payload))
            }
        }
    }
}

/// The request's body as JSON, when it is JSON.
fn json_body(body: &ResolvedBody) -> Option<Value> {
    match body {
        ResolvedBody::Json(value) => Some(value.clone()),
        ResolvedBody::JsonText(text) => serde_json::from_str(&text.text).ok(),
        _ => None,
    }
}

/// A schema-aware variant of the operation the request matches, else a
/// schema-less mutation of its body.
fn mutation(
    plan: &FuzzPlan,
    request: &ResolvedRequest,
    base: &Value,
    pick: u64,
) -> Option<(BodyOverride, Fuzzed)> {
    let path = url::Url::parse(&request.url.text)
        .map(|u| u.path().to_owned())
        .unwrap_or_default();
    let schema = plan
        .schema
        .as_ref()
        .and_then(|s| s.variants(&request.method, &path));
    if let Some(variants) = schema {
        let index = usize::try_from(pick % variants.len() as u64).ok()?;
        let variant = &variants[index];
        let text = variant.body.to_string();
        let fuzzed = Fuzzed {
            family: "schema".into(),
            label: variant.label.clone(),
            measure: None,
            expanded: text.len() as u64,
            invalid: true,
            oversized: variant.oversized,
            body: Bytes::from(text.clone()),
            input: None,
        };
        return Some((json_override(text), fuzzed));
    }
    let variant = mutate::variant(base, pick)?;
    let text = variant.body.to_text();
    let pointer = if variant.pointer.is_empty() {
        "the root"
    } else {
        &variant.pointer
    };
    let fuzzed = Fuzzed {
        family: "mutation".into(),
        label: format!("{} at {pointer}", variant.kind.name()),
        measure: None,
        expanded: text.len() as u64,
        invalid: false,
        oversized: false,
        body: Bytes::from(text.clone()),
        input: None,
    };
    Some((json_override(text), fuzzed))
}

fn json_override(text: String) -> BodyOverride {
    BodyOverride {
        bytes: Bytes::from(text),
        content_type: Some("application/json".into()),
        content_encoding: None,
    }
}

fn from_payload(payload: Payload) -> (BodyOverride, Fuzzed) {
    let label = match payload.measure {
        Some((what, n)) => format!("{what} {n}"),
        None => payload.family.name().to_owned(),
    };
    let fuzzed = Fuzzed {
        family: payload.family.name().to_owned(),
        label,
        measure: payload.measure,
        expanded: payload.expanded(),
        invalid: payload.family.is_invalid(),
        oversized: false,
        body: payload.bytes.clone(),
        input: None,
    };
    let body = BodyOverride {
        bytes: payload.bytes,
        content_type: Some(payload.content_type),
        content_encoding: payload.content_encoding,
    };
    (body, fuzzed)
}

impl Fuzzed {
    /// The start of the body as evidence: redacted whole first, so a secret
    /// that crosses the cut is never shown in part, then cut to
    /// [`PREVIEW`] bytes and escaped (invalid UTF-8, control characters).
    pub fn preview(&self, redactor: &Redactor) -> String {
        preview(redactor, &self.body)
    }
}

fn preview(redactor: &Redactor, bytes: &[u8]) -> String {
    let head = redactor.bytes_prefix(bytes, PREVIEW);
    let text: String = String::from_utf8_lossy(&head).escape_debug().collect();
    if bytes.len() > PREVIEW {
        format!("{text}… ({} bytes)", bytes.len())
    } else {
        text
    }
}

impl SendGate for StressGate {
    fn send<'a>(
        &'a self,
        request: &'a ResolvedRequest,
        options: &'a SendOptions,
    ) -> futures_util::future::BoxFuture<'a, Result<Exchange, RequestError>> {
        Box::pin(async move {
            let slot = lock(&self.armed).take();
            let variant = match slot {
                Some(slot) => self.variant(request, slot).await,
                None => None,
            };
            let Some((body, mut fuzzed)) = variant else {
                return self.inner.send(request, options).await;
            };
            fuzzed.input = Some(body.clone());
            *lock(&self.sent) = Some(fuzzed);
            let options = SendOptions {
                body_override: Some(body),
                ..options.clone()
            };
            self.inner.send(request, &options).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_are_short_and_escaped() {
        let none = Redactor::new();
        assert_eq!(
            preview(&none, b"{\"a\":\"\xff\"}"),
            "{\\\"a\\\":\\\"\u{fffd}\\\"}"
        );
        let long = vec![b'['; 1000];
        let shown = preview(&none, &long);
        assert!(shown.ends_with("… (1000 bytes)"), "{shown}");
        assert!(shown.starts_with(&"[".repeat(PREVIEW)));
    }

    #[test]
    fn json_bodies_are_read_from_values_and_text() {
        let value = serde_json::json!({"a": 1});
        assert_eq!(json_body(&ResolvedBody::Json(value.clone())), Some(value));
        assert_eq!(json_body(&ResolvedBody::None), None);
    }

    fn redactor(secret: &str) -> Redactor {
        let mut r = Redactor::new();
        r.add_value("pw", secret);
        r
    }

    #[test]
    fn a_secret_crossing_the_cut_is_never_shown_in_part() {
        let secret = "Zq9SECRETtokenVALUE1234";
        let r = redactor(secret);
        let body = format!("{{\"pad\":\"{}\",\"k\":\"{secret}\"}}", "a".repeat(234));
        let start = body.find(secret).unwrap();
        assert!(start < PREVIEW && start + secret.len() > PREVIEW);
        let shown = preview(&r, body.as_bytes());
        assert!(!shown.contains("Zq9"), "{shown}");
        assert!(!r.reveals_secret(&shown), "{shown}");
    }

    #[test]
    fn a_secret_needing_escapes_is_redacted_before_escaping() {
        let secret = "p\"w\\d-SECRET";
        let r = redactor(secret);
        let body = format!("raw {secret} end");
        let shown = preview(&r, body.as_bytes());
        assert!(!shown.contains("SECRET"), "{shown}");
    }
}
