//! Sending requests whose credentials are computed per request: a JWT
//! signed for each send, Digest's challenge and retry, and SigV4 (signed
//! hop by hop in the engine). The values they produce join
//! `request.secrets`, so every sink redacts them.

use std::time::Instant;

use rustman_core::model::Auth;
use rustman_core::vars::{ResolvedBody, ResolvedPartSource, ResolvedRequest};
use rustman_core::{Redactor, Secret};

use crate::cookies::NO_ENVIRONMENT;
use crate::digest::{self, Answer, Challenge, MAX_REPLAY_BYTES};
use crate::engine::{HttpEngine, Prepared, SendOptions, Ungated};
use crate::error::RequestError;
use crate::exchange::Exchange;
use crate::sigv4::Signer;
use crate::{jwt, wire};

impl HttpEngine {
    /// Send `request` when its auth is `jwt`, `digest` or `aws_sigv4`;
    /// `None` for any other auth. `env` is the active environment, which
    /// scopes Digest nonces.
    pub(crate) async fn send_signed(
        &self,
        request: &mut ResolvedRequest,
        options: &SendOptions,
        env: Option<&str>,
    ) -> Option<Result<Exchange, RequestError>> {
        let result = match request.auth.clone() {
            Some(Auth::Jwt(config)) => {
                let token = match jwt::sign(&config, jwt::now()) {
                    Ok(token) => Secret::new(jwt::SECRET_NAME, token),
                    Err(e) => return Some(Err(e)),
                };
                request.secrets.push(token.clone());
                self.send_with_token(request, options, Some(&token)).await
            }
            Some(Auth::Digest { username, password }) => {
                let credentials = (username.as_str(), password.as_str());
                let env = env.unwrap_or(NO_ENVIRONMENT);
                self.send_digest(request, options, env, credentials)
                    .await
                    .map_err(|e| e.redacted(&Redactor::for_request(request)))
            }
            Some(Auth::AwsSigV4(_)) => match Signer::for_auth(request.auth.as_ref()) {
                Ok(signer) => {
                    request
                        .secrets
                        .extend(signer.iter().flat_map(Signer::secrets));
                    self.send(request, options).await
                }
                Err(e) => Err(e.redacted(&Redactor::for_request(request))),
            },
            _ => return None,
        };
        Some(result)
    }

    /// RFC 7616: send (with a cached nonce when there is one); on a `401`
    /// with a Digest challenge, answer it and send once more. The second
    /// response is returned whatever its status; both round trips count in
    /// its timings.
    async fn send_digest(
        &self,
        request: &ResolvedRequest,
        options: &SendOptions,
        env: &str,
        (username, password): (&str, &str),
    ) -> Result<Exchange, RequestError> {
        check_replayable(&request.body)?;
        let started = Instant::now();
        let prepared = Self::prepare(request, None).await?;
        if prepared
            .outgoing
            .headers()
            .contains_key(http::header::AUTHORIZATION)
        {
            return self.dispatch(request, prepared, options, &Ungated).await;
        }
        let host = authority(&prepared);
        let target = DigestTarget {
            method: prepared.outgoing.method().as_str().to_owned(),
            uri: request_target(&prepared),
            username,
            password,
        };
        let mut first = copy(&prepared);
        if let Some((challenge, nc)) = self.nonces.next(env, &host) {
            target.answer(&mut first, &challenge, nc)?;
        }
        let challenged = self.dispatch(request, first, options, &Ungated).await?;
        if challenged.status != 401 {
            return Ok(challenged);
        }
        let Some(challenge) = Challenge::from_headers(&challenged.headers)? else {
            return Ok(challenged);
        };
        discard_spill(&challenged);
        let mut retry = prepared;
        target.answer(&mut retry, &challenge, 1)?;
        let remaining = options.timeout.saturating_sub(started.elapsed());
        let options = SendOptions {
            timeout: remaining,
            ..options.clone()
        };
        let mut exchange = self.dispatch(request, retry, &options, &Ungated).await?;
        self.nonces.store(env, &host, challenge);
        let first = challenged.timings.total;
        exchange.timings.auth = Some(first);
        exchange.timings.total += first;
        exchange.credentials.merge(&challenged.credentials);
        Ok(exchange)
    }
}

/// What a Digest answer covers besides the challenge.
struct DigestTarget<'a> {
    method: String,
    uri: String,
    username: &'a str,
    password: &'a str,
}

impl DigestTarget<'_> {
    /// Set the `Authorization` header answering `challenge` with count `nc`.
    fn answer(
        &self,
        prepared: &mut Prepared,
        challenge: &Challenge,
        nc: u32,
    ) -> Result<(), RequestError> {
        let cnonce = digest::cnonce();
        let answer = Answer {
            username: self.username,
            password: self.password,
            method: &self.method,
            uri: &self.uri,
            nc,
            cnonce: &cnonce,
        };
        let value = http::HeaderValue::from_str(&challenge.authorization(&answer))
            .map_err(|_| RequestError::Invalid("digest: invalid credentials".into()))?;
        prepared
            .outgoing
            .headers_mut()
            .insert(http::header::AUTHORIZATION, value);
        Ok(())
    }
}

/// Fail before sending when a file or multipart body is too large to send
/// twice.
fn check_replayable(body: &ResolvedBody) -> Result<(), RequestError> {
    let size = |path: &std::path::Path| std::fs::metadata(path).map_or(0, |m| m.len());
    let total: u64 = match body {
        ResolvedBody::File(file) => size(&file.path),
        ResolvedBody::Multipart(parts) => parts
            .iter()
            .map(|part| match &part.source {
                ResolvedPartSource::File(file) => size(&file.path),
                ResolvedPartSource::Text(text) => text.text.len() as u64,
            })
            .sum(),
        _ => 0,
    };
    if total > MAX_REPLAY_BYTES {
        return Err(digest::body_too_large());
    }
    Ok(())
}

fn copy(prepared: &Prepared) -> Prepared {
    Prepared {
        outgoing: wire::duplicate(&prepared.outgoing),
        warnings: prepared.warnings.clone(),
        signer: None,
    }
}

/// `host:port` of the request, the scope of its cached nonces.
fn authority(prepared: &Prepared) -> String {
    let uri = prepared.outgoing.uri();
    let port = uri.port_u16().unwrap_or(match uri.scheme_str() {
        Some("https") => 443,
        _ => 80,
    });
    format!("{}:{port}", uri.host().unwrap_or_default())
}

/// The path and query the answer is computed over.
fn request_target(prepared: &Prepared) -> String {
    let uri = prepared.outgoing.uri();
    uri.path_and_query()
        .map_or_else(|| "/".to_owned(), |p| p.as_str().to_owned())
}

/// Remove the spill file of the challenged response's body.
fn discard_spill(exchange: &Exchange) {
    if let Some(path) = &exchange.spill {
        // Best effort: a file that cannot be removed is only left behind.
        let _ = std::fs::remove_file(path);
    }
}
