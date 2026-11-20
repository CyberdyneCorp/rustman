//! Requests sent one at a time, outside the load: the health request
//! (before load for the baseline, during load for the kill switch, after
//! it for recovery) and the replays of minimization. They go through the
//! same safety policy as the load, with a governor of their own built from
//! the `[stress]` caps (so the run time cap of the load does not cut the
//! recovery check short), and are paced by their callers.
//!
//! A replay sends a request of the plan with a variant's bytes as its
//! body ([`ReplayGate`]); its headers, auth and cookies are the request's.

use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use futures_util::future::{BoxFuture, LocalBoxFuture};
use rustman_core::RequestRef;
use rustman_core::model::Request;
use rustman_core::run::Plan;
use rustman_core::vars::ResolvedRequest;
use rustman_http::runner::{Runner, SendGate, Sent};
use rustman_http::{BodyOverride, Exchange, RequestError, SendOptions};

use crate::metrics::{ErrorClass, describe};
use crate::safety::BenchGate;
use crate::stress::recovery::{HealthCheck, Probe};

/// The send gate of single requests: the bench gate, with an optional
/// body override for replays.
pub(crate) struct ReplayGate {
    inner: BenchGate,
    body: Mutex<Option<BodyOverride>>,
}

impl ReplayGate {
    pub fn new(inner: BenchGate) -> Self {
        Self {
            inner,
            body: Mutex::new(None),
        }
    }

    fn set(&self, body: Option<BodyOverride>) {
        *self.body.lock().unwrap_or_else(PoisonError::into_inner) = body;
    }

    fn body(&self) -> Option<BodyOverride> {
        self.body
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl SendGate for ReplayGate {
    fn send<'a>(
        &'a self,
        request: &'a ResolvedRequest,
        options: &'a SendOptions,
    ) -> BoxFuture<'a, Result<Exchange, RequestError>> {
        Box::pin(async move {
            match self.body() {
                Some(body) => {
                    let options = SendOptions {
                        body_override: Some(body),
                        ..options.clone()
                    };
                    self.inner.send(request, &options).await
                }
                None => self.inner.send(request, options).await,
            }
        })
    }
}

/// Where the health request comes from.
pub(crate) enum Health<'a> {
    /// No GET in the plan and no `--health`.
    None,
    /// A request of the plan (its first GET).
    InPlan(usize),
    /// `--health <file>`.
    File(RequestRef<'a>),
}

/// Sends single requests of the plan, one at a time.
pub(crate) struct Prober<'a> {
    runner: Runner<'a>,
    gate: &'a ReplayGate,
    plan: Plan<'a>,
    health: Health<'a>,
    /// One request at a time: the gate's body belongs to it.
    turn: tokio::sync::Mutex<()>,
}

impl<'a> Prober<'a> {
    pub fn new(
        runner: Runner<'a>,
        gate: &'a ReplayGate,
        plan: Plan<'a>,
        health: Health<'a>,
    ) -> Self {
        Self {
            runner,
            gate,
            plan,
            health,
            turn: tokio::sync::Mutex::new(()),
        }
    }

    /// The health request's file, when there is one.
    pub fn health_file(&self) -> Option<String> {
        self.health_request().map(|r| file_name(r))
    }

    fn health_request(&self) -> Option<&RequestRef<'a>> {
        match &self.health {
            Health::None => None,
            Health::InPlan(index) => self.plan.requests.get(*index),
            Health::File(request) => Some(request),
        }
    }

    /// Send request `file` of the plan, with `body` in place of its own.
    pub async fn replay(&self, file: usize, body: BodyOverride) -> Option<Sent> {
        let request = self.plan.requests.get(file)?;
        Some(self.send(request, Some(body)).await.0)
    }

    async fn send(&self, request: &RequestRef<'_>, body: Option<BodyOverride>) -> (Sent, Probe) {
        let _turn = self.turn.lock().await;
        self.gate.set(body);
        let started = Instant::now();
        let sent = self.runner.send(request, 1).await;
        let latency = started.elapsed();
        self.gate.set(None);
        let probe = probe_of(&sent, latency);
        (sent, probe)
    }

    /// Send the health request; `None` without one.
    pub async fn health(&self) -> Option<Probe> {
        let request = self.health_request()?;
        Some(self.send(request, None).await.1)
    }
}

impl HealthCheck for Prober<'_> {
    fn probe(&self) -> LocalBoxFuture<'_, Probe> {
        Box::pin(async move {
            self.health().await.unwrap_or(Probe {
                status: None,
                latency: std::time::Duration::ZERO,
                error: Some(ErrorClass::Other),
                detail: Some("no health request".into()),
            })
        })
    }
}

/// What a single request got back, redacted.
fn probe_of(sent: &Sent, latency: std::time::Duration) -> Probe {
    let status = sent.record.exchange.as_ref().map(|e| e.response.status);
    let detail = status
        .is_none()
        .then(|| sent.redactor.text(&describe(&sent.record)).into_owned());
    Probe {
        status,
        latency,
        error: ErrorClass::of(&sent.record),
        detail,
    }
}

/// The plan's first GET request, the default health request.
pub(crate) fn first_get(plan: &Plan<'_>) -> Option<usize> {
    plan.requests.iter().position(|r| match r.request() {
        Request::Http(http) => http.effective_method().eq_ignore_ascii_case("GET"),
        _ => false,
    })
}

/// A request's file, with `/`.
pub(crate) fn file_name(request: &RequestRef<'_>) -> String {
    request.entry.path.to_string_lossy().replace('\\', "/")
}
