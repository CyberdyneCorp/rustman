//! What sends the HTTP requests of an operation or an introspection: the
//! run's [`HttpEngine`] by default, or a sender that admits each request
//! first, such as the agent interface's governed sender (allowlist, rate
//! limit and the agent's mutation rule).

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;

use rustman_core::vars::ResolvedRequest;
use rustman_http::{Exchange, HttpEngine, RequestError, SendOptions};

/// The future of [`HttpSender::send`].
pub type SendFuture<'a> = Pin<Box<dyn Future<Output = Result<Exchange, RequestError>> + Send + 'a>>;

/// Sends one HTTP request of a GraphQL operation or introspection.
pub trait HttpSender: Send + Sync {
    /// Send `request`, or refuse it with [`RequestError::Blocked`]. Error
    /// messages must be redacted.
    fn send<'a>(&'a self, request: &'a ResolvedRequest, options: &'a SendOptions)
    -> SendFuture<'a>;
}

impl HttpSender for HttpEngine {
    fn send<'a>(
        &'a self,
        request: &'a ResolvedRequest,
        options: &'a SendOptions,
    ) -> SendFuture<'a> {
        Box::pin(HttpEngine::send(self, request, options))
    }
}

/// A shared [`HttpSender`], for [`crate::GraphqlRunner::sender`].
#[derive(Clone)]
pub struct SharedSender(pub Arc<dyn HttpSender>);

impl fmt::Debug for SharedSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SharedSender")
    }
}
