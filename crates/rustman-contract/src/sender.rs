//! How contract commands reach the network. Only Pact provider
//! verification sends requests, through [`GovernedSenders`] (design D6):
//! every interaction and every provider-state call passes the safety policy
//! and the governor of `rustman_http::GovernedSender`.
//!
//! Passive conformance is built with [`Offline`], which sends nothing; its
//! tests build it with a sender that panics on use instead (design D7).

use std::future::Future;

use rustman_core::safety::{
    EnvironmentInfo, Governor, Limits, Origin, Permission, PolicyError, PolicyOptions,
    SafetyPolicy, SafetySettings,
};
use rustman_core::vars::ResolvedRequest;
use rustman_http::{GovernedError, GovernedSender, SendOptions};

use crate::pact::matching::Actual;

/// Why a request produced no response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendError {
    /// The safety policy refused it: `blocked: mutations not allowed`.
    Blocked(String),
    /// Sending failed (connection refused, timeout, ...).
    Failed(String),
}

/// Sends the requests of a contract command.
pub trait Sender {
    /// One Pact interaction, to the provider.
    fn interaction(
        &self,
        request: &ResolvedRequest,
    ) -> impl Future<Output = Result<Actual, SendError>>;

    /// One provider-state setup or teardown call.
    fn state_change(
        &self,
        request: &ResolvedRequest,
    ) -> impl Future<Output = Result<Actual, SendError>>;
}

/// A sender for commands that never send anything.
#[derive(Clone, Copy, Debug, Default)]
pub struct Offline;

impl Sender for Offline {
    async fn interaction(&self, _: &ResolvedRequest) -> Result<Actual, SendError> {
        Err(SendError::Failed("this command sends no request".into()))
    }

    async fn state_change(&self, _: &ResolvedRequest) -> Result<Actual, SendError> {
        Err(SendError::Failed("this command sends no request".into()))
    }
}

/// The governed senders of provider verification: interactions under the
/// policy built from `--provider-url` (with the state-change origin also
/// allowed) and the method flags; state-change calls under a policy that
/// allows only the `--provider-states-url` origin, where `POST` is allowed
/// because the user named that endpoint for it. Both share one governor.
#[derive(Debug)]
pub struct GovernedSenders {
    interactions: GovernedSender,
    states: Option<GovernedSender>,
    options: SendOptions,
}

/// Where provider verification may send requests.
#[derive(Clone, Copy, Debug)]
pub struct Targets<'a> {
    pub provider_url: &'a str,
    pub states_url: Option<&'a str>,
    /// From `--allow-mutations` and `--allow-destructive`.
    pub permission: Permission,
    /// From `--production`: the provider host, confirmed.
    pub production: Option<&'a str>,
    pub environment: EnvironmentInfo<'a>,
}

impl GovernedSenders {
    /// The senders for `targets` under `[safety]`.
    pub fn new(settings: &SafetySettings, targets: Targets<'_>) -> Result<Self, PolicyError> {
        let states_origin = targets
            .states_url
            .map(|url| Origin::of(url).map_err(PolicyError::InvalidTarget))
            .transpose()?;
        let extra: Vec<Origin> = states_origin.into_iter().collect();
        let policy = SafetyPolicy::new(
            settings,
            PolicyOptions {
                target: targets.provider_url,
                permission: targets.permission,
                allow_hosts: &extra,
                production: targets.production,
                environment: targets.environment,
            },
        )?;
        let interactions =
            GovernedSender::new(policy, Governor::new(Limits::from_settings(settings)));
        let states = match targets.states_url {
            Some(url) => {
                let only_states = SafetySettings {
                    allowed_hosts: Vec::new(),
                    ..settings.clone()
                };
                let policy = SafetyPolicy::new(
                    &only_states,
                    PolicyOptions {
                        target: url,
                        permission: Permission::from_flags(true, false),
                        allow_hosts: &[],
                        production: targets.production,
                        environment: targets.environment,
                    },
                )?;
                Some(interactions.with_policy(policy))
            }
            None => None,
        };
        let options = SendOptions {
            follow_redirects: false,
            ..SendOptions::default()
        };
        Ok(Self {
            interactions,
            states,
            options,
        })
    }
}

impl Sender for GovernedSenders {
    async fn interaction(&self, request: &ResolvedRequest) -> Result<Actual, SendError> {
        send(&self.interactions, request, &self.options).await
    }

    async fn state_change(&self, request: &ResolvedRequest) -> Result<Actual, SendError> {
        let Some(states) = &self.states else {
            return Err(SendError::Failed("no --provider-states-url".into()));
        };
        send(states, request, &self.options).await
    }
}

async fn send(
    sender: &GovernedSender,
    request: &ResolvedRequest,
    options: &SendOptions,
) -> Result<Actual, SendError> {
    match sender.send(request, options).await {
        Ok(exchange) => Ok(Actual {
            status: exchange.status,
            headers: exchange
                .headers
                .iter()
                .map(|(n, v)| {
                    (
                        n.to_string(),
                        String::from_utf8_lossy(v.as_bytes()).into_owned(),
                    )
                })
                .collect(),
            body: exchange.body.to_vec(),
        }),
        Err(GovernedError::Blocked { reason, .. }) => Err(SendError::Blocked(reason.to_string())),
        Err(other) => Err(SendError::Failed(other.to_string())),
    }
}
