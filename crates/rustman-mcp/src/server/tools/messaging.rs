//! Messaging requests run by agents (add-messaging-client "Safety
//! gating"): `send_request` and `run_collection` run saved `kind: kafka`,
//! `kind: mqtt` and `kind: amqp` files through [`AgentMessaging`].
//!
//! Before anything connects, the operation's class is checked against
//! the agent permissions (produce needs `[agent].allow_mutations`, a
//! named-queue consume `[agent].allow_destructive`), every broker against
//! the session's allowlist (as the `http`/`https` origin of the endpoint
//! the client dials; a broker that does not parse is `invalid_input`,
//! and audits never name its raw text), and a produce
//! is confirmed by the human as for any mutation. Consumes are capped at
//! 100 messages and 60 s, and Schema Registry redirects are not followed
//! (only the registry's base URL is authorized). A refusal becomes the tool's error, so it is
//! audited as `blocked`.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use rustman_core::safety::MethodClass;
use rustman_core::vars::ResolvedMessaging;
use rustman_http::runner::{MessagingCall, MessagingExecutor, MessagingFuture, MessagingOutcome};
use rustman_messaging::endpoint::INVALID_BROKER;
use rustman_messaging::safety::{audit_target, broker_urls, check, classify, registry_url};
use rustman_messaging::{Bounds, MessagingRunner};

use crate::server::confirm::Confirm;
use crate::server::context::{AgentContext, JobCaller, Target};
use crate::server::error::{ErrorCode, ToolError};

/// The most messages an agent consume collects.
pub const MAX_MESSAGES: u64 = 100;

/// The longest an agent consume runs.
pub const MAX_DURATION: Duration = Duration::from_secs(60);

/// The messaging executor of agent runs.
pub struct AgentMessaging<'a, C: Confirm> {
    ctx: &'a AgentContext,
    confirm: &'a C,
    target: &'a Target,
    runner: MessagingRunner,
    refused: Mutex<Option<ToolError>>,
    /// The background job running, whose operations get audit lines of
    /// their own.
    job: Option<JobCaller>,
}

impl<'a, C: Confirm> AgentMessaging<'a, C> {
    pub fn new(ctx: &'a AgentContext, confirm: &'a C, target: &'a Target) -> Self {
        Self {
            ctx,
            confirm,
            target,
            runner: agent_runner(),
            refused: Mutex::new(None),
            job: None,
        }
    }

    /// The executor of a background job: each operation gets an audit line
    /// naming `job`.
    pub fn for_job(
        ctx: &'a AgentContext,
        confirm: &'a C,
        target: &'a Target,
        job: JobCaller,
    ) -> Self {
        Self {
            job: Some(job),
            ..Self::new(ctx, confirm, target)
        }
    }

    /// The first operation the agent permissions, the policy or the human
    /// refused.
    pub fn refused(&self) -> Option<ToolError> {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn refuse(&self, err: &ToolError) {
        let mut refused = self.refused.lock().unwrap_or_else(PoisonError::into_inner);
        refused.get_or_insert_with(|| err.clone());
    }

    /// Whether `resolved` may run: its class, every broker, and for a
    /// produce or a destructive consume the human's confirmation.
    async fn authorize(&self, resolved: &ResolvedMessaging) -> Result<(), ToolError> {
        let method = method_of(classify(resolved));
        let brokers = broker_urls(resolved);
        let first = brokers.as_ref().ok().and_then(|b| b.first());
        self.target
            .set(method, first.map_or(INVALID_BROKER, String::as_str));
        let brokers =
            brokers.map_err(|e| ToolError::new(ErrorCode::InvalidInput, e.to_string()))?;
        let permission = self.ctx.agent_settings().permission();
        check(permission, resolved).map_err(|r| ToolError::blocked(r.reason()))?;
        for url in &brokers {
            self.ctx.authorize(method, url, self.confirm).await?;
        }
        if let Some(registry) = registry_url(resolved) {
            self.ctx.authorize("GET", registry, self.confirm).await?;
        }
        Ok(())
    }

    /// Audit the operation when a job ran it.
    fn audit(&self, resolved: &ResolvedMessaging, error: Option<&ToolError>) {
        let Some(job) = &self.job else {
            return;
        };
        let method = method_of(classify(resolved));
        let url = audit_target(resolved);
        if let Err(err) = self.ctx.audit_job_request(job, method, &url, error) {
            tracing::warn!("audit: {err}");
        }
    }
}

impl<C: Confirm> MessagingExecutor for AgentMessaging<'_, C> {
    fn execute<'a>(&'a self, call: MessagingCall<'a>) -> MessagingFuture<'a> {
        Box::pin(async move {
            let authorized = self.authorize(call.resolved).await;
            self.audit(call.resolved, authorized.as_ref().err());
            if let Err(err) = authorized {
                self.refuse(&err);
                let mut outcome = MessagingOutcome::blocked(err.to_string());
                outcome.blocked = err.code == ErrorCode::Blocked;
                return outcome;
            }
            self.runner.execute(call).await
        })
    }
}

/// The runner of agent operations: consumes capped, and no Schema
/// Registry redirect followed.
fn agent_runner() -> MessagingRunner {
    MessagingRunner {
        ceiling: Some(Bounds {
            max_messages: MAX_MESSAGES,
            duration: MAX_DURATION,
        }),
        no_registry_redirects: true,
        ..MessagingRunner::default()
    }
}

/// The HTTP method a policy class is checked and confirmed as.
fn method_of(class: MethodClass) -> &'static str {
    match class {
        MethodClass::ReadOnly => "GET",
        MethodClass::Mutation => "POST",
        MethodClass::Destructive => "DELETE",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_runs_cap_consumes_and_do_not_follow_registry_redirects() {
        let runner = agent_runner();
        assert!(runner.no_registry_redirects);
        let ceiling = runner.ceiling.unwrap();
        assert_eq!(
            (ceiling.max_messages, ceiling.duration),
            (MAX_MESSAGES, MAX_DURATION)
        );
    }
}
