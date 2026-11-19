//! Prompts and argument completion (mcp-client spec "Prompts", "Argument
//! completion"): the paginated prompt list, `prompts/get` with its required
//! arguments checked first, and `completion/complete` behind a 250 ms
//! debounce so typing does not flood the server.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rmcp::model::{
    ArgumentInfo, ClientRequest, CompleteRequest, CompleteRequestParams, GetPromptRequest,
    GetPromptRequestParams, ListPromptsRequest, Prompt, Reference, ServerResult,
};
use serde::Serialize;
use serde_json::{Map, Value};

use super::error::ClientError;
use super::info::Operation;
use super::notify::redact_value;
use super::request::{Exchange, RequestOptions};
use super::schema::Violation;
use super::session::McpSession;
use super::tools::ContentItem;

/// How long completion waits for the user to stop typing.
pub const COMPLETION_DEBOUNCE: Duration = Duration::from_millis(250);

/// One message `prompts/get` returned.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PromptLine {
    pub role: String,
    pub content: ContentItem,
}

/// The answer to `prompts/get`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PromptResult {
    pub description: Option<String>,
    pub messages: Vec<PromptLine>,
}

/// What completion is asked for: a prompt argument or a resource-template
/// parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionTarget {
    Prompt(String),
    ResourceTemplate(String),
}

/// The required arguments of `prompt` missing from `arguments`.
pub fn missing_arguments(prompt: &Prompt, arguments: &BTreeMap<String, String>) -> Vec<Violation> {
    prompt
        .arguments
        .iter()
        .flatten()
        .filter(|a| a.required == Some(true) && !arguments.contains_key(&a.name))
        .map(|a| Violation::new(format!("/{}", a.name), "required property missing"))
        .collect()
}

impl McpSession {
    /// `prompts/list`, every page.
    pub async fn list_prompts(
        &self,
        options: &RequestOptions,
    ) -> Result<Exchange<Vec<Prompt>>, ClientError> {
        self.require(Operation::ListPrompts)?;
        self.sender()
            .paginate(
                "prompts/list",
                options,
                |p| ClientRequest::ListPromptsRequest(ListPromptsRequest::with_param(p)),
                |result| match result {
                    ServerResult::ListPromptsResult(page) => Some((page.prompts, page.next_cursor)),
                    _ => None,
                },
            )
            .await
    }

    /// `prompts/get` of `name`. When `prompt` (its listing) is given, its
    /// required arguments are checked first.
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: &BTreeMap<String, String>,
        prompt: Option<&Prompt>,
        options: &RequestOptions,
    ) -> Result<Exchange<PromptResult>, ClientError> {
        self.require(Operation::GetPrompt)?;
        let missing = prompt
            .map(|p| missing_arguments(p, arguments))
            .unwrap_or_default();
        if !missing.is_empty() {
            return Err(ClientError::InvalidArguments(missing));
        }
        let arguments: Map<String, Value> = arguments
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        let params = GetPromptRequestParams::new(name).with_arguments(arguments);
        let request = ClientRequest::GetPromptRequest(GetPromptRequest::new(params));
        let sender = self.sender();
        let exchange = sender.send(request, options).await?;
        let ServerResult::GetPromptResult(result) = &exchange.result else {
            return Err(sender.unexpected("prompts/get"));
        };
        let value = redact_value(
            &self.redactor(),
            serde_json::to_value(result).unwrap_or_default(),
        );
        let prompt = prompt_result(&value);
        Ok(exchange.map(|_| prompt))
    }

    /// `completion/complete` of `argument` = `value` for `target`.
    pub async fn complete(
        &self,
        target: &CompletionTarget,
        argument: &str,
        value: &str,
        options: &RequestOptions,
    ) -> Result<Vec<String>, ClientError> {
        self.require(Operation::Complete)?;
        let reference = match target {
            CompletionTarget::Prompt(name) => Reference::for_prompt(name),
            CompletionTarget::ResourceTemplate(uri) => Reference::for_resource(uri),
        };
        let params = CompleteRequestParams::new(reference, ArgumentInfo::new(argument, value));
        let request = ClientRequest::CompleteRequest(CompleteRequest::new(params));
        let sender = self.sender();
        let exchange = sender.send(request, options).await?;
        match exchange.result {
            ServerResult::CompleteResult(result) => Ok(result.completion.values),
            _ => Err(sender.unexpected("completion/complete")),
        }
    }
}

fn prompt_result(value: &Value) -> PromptResult {
    let messages = value
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|m| PromptLine {
            role: m
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            content: ContentItem::from_value(m.get("content").unwrap_or(&Value::Null)),
        })
        .collect();
    PromptResult {
        description: value
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        messages,
    }
}

/// Debounces completion while the user types: each keystroke calls
/// [`Debouncer::settle`], and only the call that is not superseded within
/// the delay goes on to request completion.
#[derive(Clone, Debug)]
pub struct Debouncer {
    delay: Duration,
    generation: Arc<AtomicU64>,
}

impl Default for Debouncer {
    fn default() -> Self {
        Self::new(COMPLETION_DEBOUNCE)
    }
}

impl Debouncer {
    pub fn new(delay: Duration) -> Self {
        Self {
            delay,
            generation: Arc::default(),
        }
    }

    /// Wait the delay; `true` when no later keystroke came meanwhile.
    pub async fn settle(&self) -> bool {
        let mine = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        tokio::time::sleep(self.delay).await;
        self.generation.load(Ordering::SeqCst) == mine
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn required_prompt_arguments_are_checked() {
        let prompt: Prompt = serde_json::from_value(json!({
            "name": "review",
            "arguments": [{"name": "code", "required": true}, {"name": "style"}]
        }))
        .unwrap();
        let missing = missing_arguments(&prompt, &BTreeMap::new());
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].to_string(), "/code: required property missing");
        let given = BTreeMap::from([("code".to_owned(), "x".to_owned())]);
        assert!(missing_arguments(&prompt, &given).is_empty());
    }

    #[test]
    fn prompt_messages_keep_their_roles() {
        let result = prompt_result(&json!({
            "description": "d",
            "messages": [
                {"role": "user", "content": {"type": "text", "text": "Review x"}},
                {"role": "assistant", "content": {"type": "text", "text": "ok"}}
            ]
        }));
        assert_eq!(result.messages[0].role, "user");
        assert_eq!(result.messages[1].content.summary(), "ok");
    }

    #[tokio::test(start_paused = true)]
    async fn only_the_last_keystroke_within_250ms_completes() {
        let debouncer = Debouncer::default();
        let first = tokio::spawn({
            let d = debouncer.clone();
            async move { d.settle().await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let second = tokio::spawn({
            let d = debouncer.clone();
            async move { d.settle().await }
        });
        assert!(!first.await.unwrap());
        assert!(second.await.unwrap());
        let started = tokio::time::Instant::now();
        assert!(debouncer.settle().await);
        assert_eq!(started.elapsed(), COMPLETION_DEBOUNCE);
    }
}
