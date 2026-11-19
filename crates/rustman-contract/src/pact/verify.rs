//! `rustman contract pact verify` (contract-testing "Provider
//! verification", "Provider states", "Verification passes the safety
//! policy"): each interaction is replayed against the provider through a
//! [`Sender`], with its provider states set up before and torn down after,
//! and the response is compared with [`super::matching`].

use rustman_core::model::RequestSettings;
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedPair, ResolvedRequest, TlsFiles};
use serde_json::{Value, json};

use super::matching::{Mismatch, compare};
use super::model::{Interaction, Pact, PactRequest, ProviderState};
use crate::sender::{SendError, Sender};

/// The message when stateful interactions have nowhere to set their state.
pub const STATES_URL_MISSING: &str = "provider states declared but --provider-states-url not set";

/// How one interaction ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    /// Every mismatch, or why no response could be compared.
    Failed(Vec<String>),
    /// `blocked: <reason>`: the safety policy refused a request.
    Blocked(String),
}

impl Outcome {
    pub fn status(&self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed(_) => "failed",
            Self::Blocked(_) => "blocked",
        }
    }
}

/// One verified interaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    /// The Pact file, as given.
    pub file: String,
    /// The interaction's index in the file.
    pub index: usize,
    pub consumer: String,
    pub description: String,
    pub outcome: Outcome,
}

/// Every interaction's result, and problems that did not decide one
/// (a failed teardown).
#[derive(Clone, Debug, Default)]
pub struct Verification {
    pub results: Vec<Verified>,
    pub warnings: Vec<String>,
}

impl Verification {
    pub fn count(&self, status: &str) -> usize {
        self.results
            .iter()
            .filter(|r| r.outcome.status() == status)
            .count()
    }

    /// `2 passed, 1 failed, 0 blocked`.
    pub fn summary(&self) -> String {
        format!(
            "{} passed, {} failed, {} blocked",
            self.count("passed"),
            self.count("failed"),
            self.count("blocked")
        )
    }
}

/// Replays Pact interactions.
pub struct Verifier<'a, S> {
    pub sender: &'a S,
    pub provider_url: &'a str,
    pub states_url: Option<&'a str>,
}

impl<S: Sender> Verifier<'_, S> {
    /// Verify every interaction of `pacts` (file name and Pact), in order.
    /// Fails before sending anything when an interaction declares provider
    /// states and there is no state-change URL.
    pub async fn verify(&self, pacts: &[(String, Pact)]) -> Result<Verification, String> {
        let stateful = pacts
            .iter()
            .flat_map(|(_, p)| &p.interactions)
            .any(|i| !i.provider_states.is_empty());
        if stateful && self.states_url.is_none() {
            return Err(STATES_URL_MISSING.to_owned());
        }
        let mut out = Verification::default();
        for (file, pact) in pacts {
            for (index, interaction) in pact.interactions.iter().enumerate() {
                let outcome = self
                    .interaction(&pact.consumer, interaction, &mut out.warnings)
                    .await;
                out.results.push(Verified {
                    file: file.clone(),
                    index,
                    consumer: pact.consumer.clone(),
                    description: interaction.description.clone(),
                    outcome,
                });
            }
        }
        Ok(out)
    }

    async fn interaction(
        &self,
        consumer: &str,
        interaction: &Interaction,
        warnings: &mut Vec<String>,
    ) -> Outcome {
        if let Err(reason) = check_path(&interaction.request.path) {
            return Outcome::Failed(vec![reason]);
        }
        let states = &interaction.provider_states;
        let mut set_up = 0;
        let mut outcome = None;
        for state in states {
            if let Err(failed) = self.state(consumer, state, "setup").await {
                outcome = Some(failed);
                break;
            }
            set_up += 1;
        }
        let outcome = match outcome {
            Some(failed) => failed,
            None => self.replay(interaction).await,
        };
        for state in states[..set_up].iter().rev() {
            if let Err(failed) = self.state(consumer, state, "teardown").await {
                let detail = describe(&failed);
                let what = &interaction.description;
                warnings.push(format!("{what}: {detail}"));
            }
        }
        outcome
    }

    /// POST `{"consumer", "state", "params", "action"}` to the state URL.
    async fn state(
        &self,
        consumer: &str,
        state: &ProviderState,
        action: &str,
    ) -> Result<(), Outcome> {
        let url = self.states_url.unwrap_or_default();
        let body = json!({
            "consumer": consumer,
            "state": state.name,
            "params": state.params,
            "action": action,
        });
        let headers = vec![("Content-Type".to_owned(), "application/json".to_owned())];
        let request = resolved("POST", url.to_owned(), &headers, Some(&body));
        let name = &state.name;
        match self.sender.state_change(&request).await {
            Ok(actual) if (200..300).contains(&actual.status) => Ok(()),
            Ok(actual) => Err(Outcome::Failed(vec![format!(
                "provider state '{name}': {action} returned {}",
                actual.status
            )])),
            Err(SendError::Blocked(reason)) => Err(Outcome::Blocked(reason)),
            Err(SendError::Failed(e)) => Err(Outcome::Failed(vec![format!(
                "provider state '{name}': {action} failed: {e}"
            )])),
        }
    }

    async fn replay(&self, interaction: &Interaction) -> Outcome {
        let request = interaction_request(self.provider_url, &interaction.request);
        match self.sender.interaction(&request).await {
            Ok(actual) => {
                let mismatches = compare(&interaction.response, &actual);
                if mismatches.is_empty() {
                    Outcome::Passed
                } else {
                    Outcome::Failed(mismatches.iter().map(Mismatch::to_string).collect())
                }
            }
            Err(SendError::Blocked(reason)) => Outcome::Blocked(reason),
            Err(SendError::Failed(e)) => Outcome::Failed(vec![format!("request failed: {e}")]),
        }
    }
}

fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Passed => "passed".to_owned(),
        Outcome::Failed(messages) => messages.join("; "),
        Outcome::Blocked(reason) => reason.clone(),
    }
}

/// A Pact request path is appended to the provider URL, so one that does
/// not start with `/` (such as `@other-host/x`) could change the host the
/// request goes to. Such an interaction is refused before anything is sent.
fn check_path(path: &str) -> Result<(), String> {
    if path.starts_with('/') {
        Ok(())
    } else {
        Err(format!(
            "request path '{path}' does not start with '/'; the interaction is not sent"
        ))
    }
}

/// The interaction's request, sent to the provider.
pub fn interaction_request(provider_url: &str, request: &PactRequest) -> ResolvedRequest {
    let mut url = format!("{}{}", provider_url.trim_end_matches('/'), request.path);
    if !request.query.is_empty() {
        let mut query = form_urlencoded::Serializer::new(String::new());
        for (name, values) in &request.query {
            for value in values {
                query.append_pair(name, value);
            }
        }
        url.push('?');
        url.push_str(&query.finish());
    }
    resolved(
        &request.method,
        url,
        &request.headers,
        request.body.as_ref(),
    )
}

fn resolved(
    method: &str,
    url: String,
    headers: &[(String, String)],
    body: Option<&Value>,
) -> ResolvedRequest {
    let body = match body {
        None => ResolvedBody::None,
        Some(Value::String(text)) => ResolvedBody::Text(Resolved::literal(text.clone())),
        Some(value) => ResolvedBody::JsonText(Resolved::literal(value.to_string())),
    };
    ResolvedRequest {
        method: method.to_owned(),
        url: Resolved::literal(url),
        query: Vec::new(),
        headers: headers
            .iter()
            .map(|(name, value)| ResolvedPair {
                name: Resolved::literal(name.clone()),
                value: Resolved::literal(value.clone()),
            })
            .collect(),
        body,
        auth: None,
        settings: RequestSettings::default(),
        tls: TlsFiles::default(),
        secrets: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_go_to_the_provider_with_their_query() {
        let request = PactRequest {
            method: "GET".into(),
            path: "/users/42".into(),
            query: vec![("q".into(), vec!["a b".into(), "c".into()])],
            ..PactRequest::default()
        };
        let resolved = interaction_request("http://127.0.0.1:9/", &request);
        assert_eq!(resolved.url.text, "http://127.0.0.1:9/users/42?q=a+b&q=c");
        assert_eq!(resolved.body, ResolvedBody::None);
    }

    #[test]
    fn the_summary_counts_every_outcome() {
        let result = |outcome| Verified {
            file: "p.json".into(),
            index: 0,
            consumer: "c".into(),
            description: "d".into(),
            outcome,
        };
        let verification = Verification {
            results: vec![
                result(Outcome::Passed),
                result(Outcome::Failed(vec!["x".into()])),
                result(Outcome::Blocked("blocked: mutations not allowed".into())),
                result(Outcome::Passed),
            ],
            warnings: Vec::new(),
        };
        assert_eq!(verification.summary(), "2 passed, 1 failed, 1 blocked");
    }
}
