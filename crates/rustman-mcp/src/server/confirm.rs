//! Human confirmation of mutating calls through MCP elicitation (spec "Human
//! confirmation of mutations", design D5). Fails closed: a client without
//! elicitation, or one that errors, never counts as a yes.

use std::future::Future;
use std::time::Duration;

use rmcp::model::{ElicitRequestParams, ElicitationAction, ElicitationSchema};
use rmcp::service::{ElicitationMode, Peer, RoleServer};

/// How long the human has to answer.
pub const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(300);

/// What asking the human produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirmation {
    /// The human accepted with `confirm: true`.
    Confirmed,
    /// The human declined, dismissed the request or answered `confirm: false`.
    Declined,
    /// The client did not advertise elicitation.
    NotSupported,
    /// The client advertised elicitation but the request failed.
    Failed,
}

/// Something that can ask the human a yes/no question.
pub trait Confirm: Sync {
    fn confirm(&self, message: &str) -> impl Future<Output = Confirmation> + Send;
}

/// The question's schema: one required boolean, `confirm`.
fn schema() -> ElicitationSchema {
    ElicitationSchema::builder()
        .required_bool("confirm")
        .build()
        .expect("a schema with one boolean property is valid")
}

/// Map the human's answer: only an accept carrying `confirm: true` confirms.
pub fn answer(action: ElicitationAction, content: Option<&serde_json::Value>) -> Confirmation {
    let confirmed = content
        .and_then(|c| c.get("confirm"))
        .and_then(|c| c.as_bool());
    match (action, confirmed) {
        (ElicitationAction::Accept, Some(true)) => Confirmation::Confirmed,
        _ => Confirmation::Declined,
    }
}

impl Confirm for Peer<RoleServer> {
    async fn confirm(&self, message: &str) -> Confirmation {
        if !self
            .supported_elicitation_modes()
            .contains(&ElicitationMode::Form)
        {
            return Confirmation::NotSupported;
        }
        let params = ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: message.to_owned(),
            requested_schema: schema(),
        };
        match self
            .create_elicitation_with_timeout(params, Some(CONFIRMATION_TIMEOUT))
            .await
        {
            Ok(result) => answer(result.action, result.content.as_ref()),
            Err(err) => {
                tracing::warn!("confirmation request failed: {err}");
                Confirmation::Failed
            }
        }
    }
}

/// The question shown to the human for a mutating request. `url` must
/// already be redacted.
pub fn question(method: &str, url: &str, environment: Option<&str>) -> String {
    let environment = environment.map_or_else(
        || "no environment".to_owned(),
        |name| format!("environment '{name}'"),
    );
    format!("An agent wants to send {method} {url} ({environment}). Allow it?")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn only_an_explicit_yes_confirms() {
        let yes = json!({"confirm": true});
        let no = json!({"confirm": false});
        assert_eq!(
            answer(ElicitationAction::Accept, Some(&yes)),
            Confirmation::Confirmed
        );
        for (action, content) in [
            (ElicitationAction::Accept, Some(&no)),
            (ElicitationAction::Accept, None),
            (ElicitationAction::Decline, Some(&yes)),
            (ElicitationAction::Cancel, None),
        ] {
            assert_eq!(answer(action, content), Confirmation::Declined);
        }
    }

    #[test]
    fn the_question_names_method_url_and_environment() {
        assert_eq!(
            question("DELETE", "http://127.0.0.1/users/7", Some("local")),
            "An agent wants to send DELETE http://127.0.0.1/users/7 (environment 'local'). Allow it?"
        );
        assert!(question("PUT", "http://x/", None).contains("(no environment)"));
        let schema = serde_json::to_value(schema()).unwrap();
        assert_eq!(schema["properties"]["confirm"]["type"], "boolean");
        assert_eq!(schema["required"], json!(["confirm"]));
    }
}
