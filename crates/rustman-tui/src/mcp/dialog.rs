//! The approval dialogs of the inspector (mcp-client "Sampling requires
//! human approval", "Elicitation requires human approval"): a sampling
//! request is shown in full (messages, system prompt, model preferences,
//! max tokens) and answered only with what the user typed, or rejected; an
//! elicitation shows the server's message and a form from its requested
//! schema, accepted only with content that passes it, declined or
//! cancelled. Nothing here answers on the user's behalf.

use std::time::UNIX_EPOCH;

use rustman_mcp::client::{
    Approval, ElicitationDecision, ElicitationRequest, Notification, SamplingDecision,
    SamplingRequest,
};
use serde_json::Value;

use super::form::{Form, Target};

/// A sampling or elicitation request waiting for the user.
#[derive(Debug)]
pub struct Waiting {
    /// `None` once answered.
    approval: Option<Approval>,
    /// The sampling response being typed.
    pub response: String,
    /// The elicitation form.
    pub form: Option<Form>,
}

impl Waiting {
    pub fn new(approval: Approval) -> Self {
        let form = match &approval {
            Approval::Elicitation { request, .. } => request
                .form()
                .map(|field| Form::of_schema(Target::Elicitation, &field)),
            Approval::Sampling { .. } => None,
        };
        Self {
            approval: Some(approval),
            response: String::new(),
            form,
        }
    }

    pub fn is_sampling(&self) -> bool {
        matches!(self.approval, Some(Approval::Sampling { .. }))
    }

    /// The dialog title.
    pub fn title(&self) -> String {
        match &self.approval {
            Some(Approval::Sampling { request, .. }) => {
                format!(" {} asks to sample an LLM ", request.server)
            }
            Some(Approval::Elicitation { request, .. }) => {
                format!(" {} asks you something ", request.server)
            }
            None => " Answered ".to_owned(),
        }
    }

    /// What the request says, line by line.
    pub fn lines(&self) -> Vec<String> {
        match &self.approval {
            Some(Approval::Sampling { request, .. }) => sampling_lines(request),
            Some(Approval::Elicitation { request, .. }) => elicitation_lines(request),
            None => Vec::new(),
        }
    }

    /// Send the typed response.
    pub fn respond(&mut self) {
        if let Some(Approval::Sampling { reply, .. }) = self.approval.take() {
            reply.send(SamplingDecision::typed(&self.response));
        }
    }

    /// Reject a sampling request, decline or cancel an elicitation.
    pub fn refuse(&mut self, cancel: bool) {
        match self.approval.take() {
            Some(Approval::Sampling { reply, .. }) => reply.send(SamplingDecision::Reject),
            Some(Approval::Elicitation { reply, .. }) => reply.send(if cancel {
                ElicitationDecision::Cancel
            } else {
                ElicitationDecision::Decline
            }),
            None => {}
        }
    }

    /// Accept an elicitation with the form's content, once it passes the
    /// requested schema; else the reasons, and the request stays open.
    pub fn accept(&mut self) -> Result<(), Vec<String>> {
        let Some(Approval::Elicitation { request, .. }) = &self.approval else {
            return Err(vec!["not an elicitation".to_owned()]);
        };
        let decision = match (&self.form, request.accept_url()) {
            (_, Some(decision)) => decision,
            (Some(form), None) => {
                let content = form.value()?;
                request
                    .accept(content)
                    .map_err(|v| v.iter().map(ToString::to_string).collect::<Vec<_>>())?
            }
            (None, None) => request
                .accept(Value::Object(serde_json::Map::new()))
                .map_err(|v| v.iter().map(ToString::to_string).collect::<Vec<_>>())?,
        };
        if let Some(Approval::Elicitation { reply, .. }) = self.approval.take() {
            reply.send(decision);
        }
        Ok(())
    }
}

fn sampling_lines(request: &SamplingRequest) -> Vec<String> {
    let params = &request.params;
    let mut lines = vec![
        format!(
            "System prompt: {}",
            params.system_prompt.as_deref().unwrap_or("(none)")
        ),
        format!(
            "Model preferences: {}",
            params
                .model_preferences
                .as_ref()
                .and_then(|p| serde_json::to_string(p).ok())
                .unwrap_or_else(|| "(none)".to_owned())
        ),
        format!("Max tokens: {}", params.max_tokens),
        "Messages:".to_owned(),
    ];
    for message in &params.messages {
        let value = serde_json::to_value(message).unwrap_or_default();
        let role = value["role"].as_str().unwrap_or_default().to_owned();
        let content = &value["content"];
        let text = content["text"]
            .as_str()
            .map_or_else(|| content.to_string(), str::to_owned);
        lines.push(format!("  {role}: {text}"));
    }
    lines
}

fn elicitation_lines(request: &ElicitationRequest) -> Vec<String> {
    let mut lines = vec![request.message.clone()];
    if let Some(url) = &request.url {
        lines.push(format!("Open: {url}"));
    }
    lines
}

/// `12:30:05 log info an info message`, the stream's line for `note`.
pub fn note_line(note: &Notification) -> String {
    let secs = note
        .at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let offer = match note.reread_uri() {
        Some(_) => "  (r: read again)",
        None => "",
    };
    format!("{h:02}:{m:02}:{s:02} {}{offer}", note.summary())
}
