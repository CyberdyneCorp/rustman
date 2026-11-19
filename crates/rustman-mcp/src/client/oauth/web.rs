//! The OAuth flow's own HTTP requests (metadata, registration, tokens),
//! sent through the [`HttpEngine`] without the MCP server's headers,
//! cookies or redirects: a redirect would carry codes and secrets to
//! wherever `Location` points.

use std::sync::Arc;

use rustman_core::Secret;
use rustman_core::model::RequestSettings;
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedPair, ResolvedRequest, TlsFiles};
use rustman_http::{HttpEngine, SendOptions};
use serde::de::DeserializeOwned;

#[derive(Clone)]
pub(crate) struct Web {
    engine: Arc<HttpEngine>,
    options: SendOptions,
}

/// A response: status and body.
pub(crate) struct Answer {
    pub status: u16,
    pub body: Vec<u8>,
}

fn pair(name: &str, value: &str) -> ResolvedPair {
    ResolvedPair {
        name: Resolved::literal(name),
        value: Resolved::literal(value),
    }
}

impl Web {
    pub fn new(engine: Arc<HttpEngine>, options: &SendOptions) -> Self {
        Self {
            engine,
            options: SendOptions {
                cookies: None,
                follow_redirects: false,
                ..options.clone()
            },
        }
    }

    async fn send(
        &self,
        method: &str,
        url: &str,
        body: ResolvedBody,
        secrets: Vec<Secret<String>>,
    ) -> Result<Answer, String> {
        let request = ResolvedRequest {
            method: method.to_owned(),
            url: Resolved::literal(url),
            query: Vec::new(),
            headers: vec![pair("Accept", "application/json")],
            body,
            auth: None,
            settings: RequestSettings::default(),
            tls: TlsFiles::default(),
            secrets,
        };
        let exchange = self
            .engine
            .send(&request, &self.options)
            .await
            .map_err(|e| e.to_string())?;
        Ok(Answer {
            status: exchange.status,
            body: exchange.body.to_vec(),
        })
    }

    /// GET `url` and parse its JSON body; any status but 2xx fails.
    pub async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, String> {
        let answer = self
            .send("GET", url, ResolvedBody::None, Vec::new())
            .await?;
        if !(200..300).contains(&answer.status) {
            return Err(format!("{url} answered {}", answer.status));
        }
        serde_json::from_slice(&answer.body).map_err(|e| format!("{url}: invalid JSON: {e}"))
    }

    /// POST a form; `secrets` are the values in it to redact.
    pub async fn post_form(
        &self,
        url: &str,
        form: &[(&str, String)],
        secrets: Vec<Secret<String>>,
    ) -> Result<Answer, String> {
        let fields = form.iter().map(|(n, v)| pair(n, v)).collect();
        self.send("POST", url, ResolvedBody::Form(fields), secrets)
            .await
    }

    pub async fn post_json(&self, url: &str, body: serde_json::Value) -> Result<Answer, String> {
        self.send("POST", url, ResolvedBody::Json(body), Vec::new())
            .await
    }
}
