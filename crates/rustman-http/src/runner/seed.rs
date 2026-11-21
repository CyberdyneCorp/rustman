//! Seeding a run's redactor before anything is sent: every secret the
//! requests of the run reference is known to every sink from the first
//! request on, so a server echoing a secret that only a later request uses
//! (or that the request does not send at all) cannot leak it.
//!
//! Each request is resolved once against the environment, with any name no
//! source defines resolving to nothing (captures do not exist yet). Only
//! the secrets this resolution looks up are read from the store, so no
//! unrelated keychain entry is touched.

use std::sync::atomic::Ordering;
use std::sync::{Mutex, PoisonError};

use rustman_core::model::Request;
use rustman_core::secret::Secret;
use rustman_core::vars::{
    Layer, Lookup, Resolver, SecretLookup, VarSource, resolve_graphql, resolve_grpc,
    resolve_jsonrpc, resolve_mcp, resolve_messaging, resolve_realtime, resolve_request,
};
use rustman_core::workspace::RequestRef;

use super::Runner;

impl Runner<'_> {
    /// Make every sink of this run redact the secrets `requests` reference.
    /// Done once per run: [`Runner::run`] seeds its whole plan, and
    /// [`Runner::send`] the request it sends, when nothing was seeded yet.
    pub fn seed(&self, requests: &[RequestRef<'_>]) {
        if self.seeded.swap(true, Ordering::SeqCst) || self.ctx.environment.is_none() {
            return;
        }
        let recording = Recording {
            store: self.ctx.secrets,
            seen: Mutex::default(),
        };
        for request in requests {
            let resolver =
                Resolver::for_request(self.ctx.settings, request, self.ctx.environment, &recording)
                    .with(Anything);
            match request.request() {
                Request::Graphql(_) => drop(resolve_graphql(request, &resolver)),
                Request::Jsonrpc(_) => drop(resolve_jsonrpc(request, &resolver)),
                Request::Mcp(_) => drop(resolve_mcp(request, &resolver)),
                Request::Grpc(_) => drop(resolve_grpc(request, &resolver)),
                Request::Ws(_) | Request::Sse(_) => drop(resolve_realtime(request, &resolver)),
                Request::Messaging(_) => drop(resolve_messaging(request, &resolver)),
                _ => drop(resolve_request(request, &resolver)),
            }
        }
        let seen = recording.seen.into_inner();
        self.remember(&seen.unwrap_or_else(PoisonError::into_inner));
        self.keep_as_baseline();
    }
}

/// The secret store, noting every value read through it.
struct Recording<'a> {
    store: &'a dyn SecretLookup,
    seen: Mutex<Vec<Secret<String>>>,
}

impl SecretLookup for Recording<'_> {
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
        let value = self.store.secret(env, name);
        if let Ok(Some(value)) = &value {
            let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
            seen.push(Secret::new(name, value.clone()));
        }
        value
    }
}

/// Defines every name, as empty text, below every other source: a name
/// only a capture will set does not stop the resolution.
struct Anything;

impl VarSource for Anything {
    fn layer(&self) -> Layer {
        Layer::Settings
    }

    fn lookup(&self, _name: &str) -> Option<Lookup> {
        Some(Lookup::Value(String::new()))
    }
}
