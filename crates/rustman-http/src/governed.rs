//! `GovernedSender`: the only way automated senders (exploration, agents)
//! reach the network (design D5). Every request passes the
//! [`SafetyPolicy`] before it leaves the process, and so does every redirect
//! hop; the [`Governor`] admits each probe against the budgets and the
//! concurrency limit, paces every hop to the rate limit, and backs off on
//! `429`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use bytes::Bytes;
use http::header::RETRY_AFTER;
use http::{HeaderMap, Request, StatusCode};
use http_body_util::Full;
use rustman_core::Redactor;
use rustman_core::safety::{Backoff, Blocked, BudgetExhausted, Governor, Paced, SafetyPolicy};
use rustman_core::vars::ResolvedRequest;

use crate::engine::{Gate, HttpEngine, SendOptions};
use crate::error::RequestError;
use crate::exchange::Exchange;

/// Why a governed request produced no exchange.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GovernedError {
    /// The policy refused the request or one of its redirect hops; nothing
    /// was sent to `url`. Shown as `blocked: <reason>`.
    #[error("{reason}")]
    Blocked {
        reason: Blocked,
        /// The refused URL, redacted.
        url: String,
    },
    /// A budget ran out before the request (or a redirect hop) was sent.
    /// Shown as `not run: budget`.
    #[error("{0}")]
    NotRun(BudgetExhausted),
    /// Sending failed.
    #[error(transparent)]
    Request(#[from] RequestError),
}

impl GovernedError {
    fn redacted(self, redactor: &Redactor) -> Self {
        match self {
            Self::Blocked { reason, url } => Self::Blocked {
                reason,
                url: redactor.text(&url).into_owned(),
            },
            Self::Request(e) => Self::Request(e.redacted(redactor)),
            not_run @ Self::NotRun(_) => not_run,
        }
    }
}

/// Sends requests under a [`SafetyPolicy`] and a [`Governor`]. Redirects are
/// followed by hand so that each hop is checked and paced before it is sent.
pub struct GovernedSender {
    engine: HttpEngine,
    policy: SafetyPolicy,
    governor: Governor,
    backoffs: AtomicU64,
}

impl std::fmt::Debug for GovernedSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GovernedSender")
            .field("policy", &self.policy)
            .field("governor", &self.governor)
            .finish_non_exhaustive()
    }
}

impl GovernedSender {
    pub fn new(policy: SafetyPolicy, governor: Governor) -> Self {
        Self {
            engine: HttpEngine::new(),
            policy,
            governor,
            backoffs: AtomicU64::new(0),
        }
    }

    pub fn policy(&self) -> &SafetyPolicy {
        &self.policy
    }

    pub fn governor(&self) -> &Governor {
        &self.governor
    }

    /// How many `429` responses made the sender back off so far.
    pub fn backoffs(&self) -> u64 {
        self.backoffs.load(Ordering::SeqCst)
    }

    /// Send `request` as one probe: check it against the policy, admit it
    /// against the budgets and concurrency limit, pace it, then send it and
    /// each redirect hop the policy allows. The total timeout is capped at
    /// the time left in the time budget. Error messages are redacted.
    pub async fn send(
        &self,
        request: &ResolvedRequest,
        options: &SendOptions,
    ) -> Result<Exchange, GovernedError> {
        self.send_unredacted(request, options)
            .await
            .map_err(|e| e.redacted(&Redactor::for_request(request)))
    }

    async fn send_unredacted(
        &self,
        request: &ResolvedRequest,
        options: &SendOptions,
    ) -> Result<Exchange, GovernedError> {
        let (policy, governor) = (&self.policy, &self.governor);
        let prepared = HttpEngine::prepare(request, None).await?;
        check(policy, &prepared.outgoing)?;
        let _admission = governor.admit().await.map_err(GovernedError::NotRun)?;
        let paced = governor.pace().await.map_err(GovernedError::NotRun)?;
        let mut options = options.clone();
        options.timeout = options.timeout.min(governor.remaining());
        let gate = PolicyGate {
            sender: self,
            paced: Mutex::new(paced),
        };
        self.engine
            .dispatch(request, prepared, &options, &gate)
            .await
    }

    fn record(&self, backoff: Option<Backoff>) {
        if backoff.is_some() {
            self.backoffs.fetch_add(1, Ordering::SeqCst);
        }
    }
}

fn check(policy: &SafetyPolicy, request: &Request<Full<Bytes>>) -> Result<(), GovernedError> {
    let url = request.uri().to_string();
    policy
        .check(request.method().as_str(), &url)
        .map_err(|reason| GovernedError::Blocked { reason, url })
}

/// The policy check and pacing of one probe's redirect hops.
struct PolicyGate<'a> {
    sender: &'a GovernedSender,
    /// How the hop in flight was paced.
    paced: Mutex<Paced>,
}

impl Gate for PolicyGate<'_> {
    type Error = GovernedError;

    async fn before_redirect(&self, request: &Request<Full<Bytes>>) -> Result<(), GovernedError> {
        check(self.sender.policy(), request)?;
        let paced = self.sender.governor().pace().await;
        let paced = paced.map_err(GovernedError::NotRun)?;
        *self.paced.lock().unwrap_or_else(PoisonError::into_inner) = paced;
        Ok(())
    }

    fn after_hop(&self, status: StatusCode, headers: &HeaderMap) {
        let paced = *self.paced.lock().unwrap_or_else(PoisonError::into_inner);
        let retry_after = headers.get(RETRY_AFTER).and_then(|v| v.to_str().ok());
        let backoff = self
            .sender
            .governor()
            .observe(paced, status.as_u16(), retry_after);
        self.sender.record(backoff);
    }
}
