//! A Confluent-compatible Schema Registry client (messaging-requests
//! "Schema Registry decoding", design D4): a small layer over
//! `rustman-http` (auth and TLS come from the request's
//! `schema_registry` settings) with a per-run cache of schemas by id and
//! by subject version.

mod schema;
mod wire;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustman_core::Redactor;
use rustman_core::settings::Defaults;
use rustman_core::vars::{Resolved, ResolvedPair, ResolvedRegistry, ResolvedRequest};
use rustman_http::{HttpEngine, SendOptions, TlsSettings};
use serde_json::Value;

pub use schema::RegistrySchema;
pub use wire::{header as wire_header, split as split_wire};

use crate::error::MessagingError;

/// How long one registry request may take.
const TIMEOUT: Duration = Duration::from_secs(10);

const ACCEPT: &str = "application/vnd.schemaregistry.v1+json";

type Cached = Result<Arc<RegistrySchema>, String>;

/// A subject's schema with its id, or why it could not be fetched.
type SubjectSchema = Result<(u32, Arc<RegistrySchema>), String>;

/// A decoded wire-format payload.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    pub schema_id: u32,
    pub value: Value,
}

/// The registry of one run.
pub struct RegistryClient {
    engine: HttpEngine,
    base: ResolvedRequest,
    options: SendOptions,
    redactor: Redactor,
    subject: Option<String>,
    version: String,
    by_id: Mutex<HashMap<u32, Cached>>,
    by_subject: Mutex<Option<SubjectSchema>>,
}

impl RegistryClient {
    pub fn new(registry: &ResolvedRegistry) -> Result<Self, MessagingError> {
        let http = &registry.http;
        let mut options = SendOptions::new(&Defaults::default(), &http.settings);
        options.timeout = TIMEOUT;
        let password = http.settings.client_pkcs12_password.as_deref();
        options.tls = TlsSettings::load(&http.tls, password, options.tls.verify)
            .map_err(|e| MessagingError::Tls(e.to_string()))?;
        Ok(Self {
            engine: HttpEngine::new(),
            base: http.clone(),
            options,
            redactor: Redactor::for_request(http),
            subject: registry.subject.clone(),
            version: registry
                .version
                .clone()
                .unwrap_or_else(|| "latest".to_owned()),
            by_id: Mutex::new(HashMap::new()),
            by_subject: Mutex::new(None),
        })
    }

    /// Decode `bytes` when they are in the wire format. `None` for other
    /// bytes; `Some(Err(warning))` when the schema cannot be fetched or
    /// the payload does not match it.
    pub async fn decode(&self, bytes: &[u8]) -> Option<Result<Decoded, String>> {
        let (schema_id, payload) = split_wire(bytes)?;
        let decoded = match self.schema(schema_id).await {
            Ok(schema) => schema
                .decode(payload)
                .map(|value| Decoded { schema_id, value }),
            Err(e) => Err(e),
        };
        Some(decoded.map_err(|e| {
            format!("schema registry: schema id {schema_id}: {e}; showing the raw bytes")
        }))
    }

    /// `value` encoded in the wire format with the configured subject's
    /// schema, with that schema's id. `None` when no subject is set.
    pub async fn encode(&self, value: &Value) -> Option<Result<(u32, Vec<u8>), MessagingError>> {
        let subject = self.subject.as_deref()?;
        let encoded = self.subject_schema(subject).await.and_then(|(id, schema)| {
            let mut bytes = wire_header(id);
            bytes.extend(schema.encode(value)?);
            Ok((id, bytes))
        });
        Some(encoded.map_err(|e| {
            MessagingError::Invalid(format!("schema registry: subject '{subject}': {e}"))
        }))
    }

    /// The schema with `id`, fetched once per run.
    pub async fn schema(&self, id: u32) -> Cached {
        if let Some(cached) = self.lock_ids().get(&id) {
            return cached.clone();
        }
        let fetched = self
            .fetch(&format!("/schemas/ids/{id}"))
            .await
            .map(|(_, s)| s);
        self.lock_ids().insert(id, fetched.clone());
        fetched
    }

    async fn subject_schema(&self, subject: &str) -> SubjectSchema {
        if let Some(cached) = self.lock_subject().as_ref() {
            return cached.clone();
        }
        let path = format!("/subjects/{subject}/versions/{}", self.version);
        let fetched = self.fetch(&path).await.and_then(|(id, schema)| {
            let id = id.ok_or("the registry answer has no schema id")?;
            self.lock_ids().insert(id, Ok(schema.clone()));
            Ok((id, schema))
        });
        *self.lock_subject() = Some(fetched.clone());
        fetched
    }

    /// GET `path` and parse the schema (and id) it returns.
    async fn fetch(&self, path: &str) -> Result<(Option<u32>, Arc<RegistrySchema>), String> {
        let mut request = self.base.clone();
        let base = request.url.text.trim_end_matches('/').to_owned();
        request.url = Resolved::literal(format!("{base}{path}"));
        request.headers.push(ResolvedPair {
            name: Resolved::literal("Accept"),
            value: Resolved::literal(ACCEPT),
        });
        let exchange = self
            .engine
            .send(&request, &self.options)
            .await
            .map_err(|e| self.redactor.text(&e.to_string()).into_owned())?;
        if exchange.status != 200 {
            return Err(format!("the registry answered HTTP {}", exchange.status));
        }
        let body: Value = serde_json::from_slice(&exchange.body)
            .map_err(|e| format!("the registry answer is not JSON: {e}"))?;
        parse_answer(&body)
    }

    fn lock_ids(&self) -> std::sync::MutexGuard<'_, HashMap<u32, Cached>> {
        self.by_id.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_subject(&self) -> std::sync::MutexGuard<'_, Option<SubjectSchema>> {
        self.by_subject
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// The schema of a `/schemas/ids/<id>` or `/subjects/.../versions/...`
/// answer: `schema` text, `schemaType` (absent for Avro) and `id`.
fn parse_answer(body: &Value) -> Result<(Option<u32>, Arc<RegistrySchema>), String> {
    let text = body
        .get("schema")
        .and_then(Value::as_str)
        .ok_or("the registry answer has no schema")?;
    let schema_type = body.get("schemaType").and_then(Value::as_str);
    let id = body
        .get("id")
        .and_then(Value::as_u64)
        .and_then(|id| u32::try_from(id).ok());
    let schema = RegistrySchema::parse(schema_type, text)?;
    Ok((id, Arc::new(schema)))
}
