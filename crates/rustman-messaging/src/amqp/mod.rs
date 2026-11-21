//! The AMQP 0-9-1 client (messaging-requests "AMQP publish", "AMQP
//! consume"; design D3) on `lapin`. The TCP stream is opened here and
//! upgraded with the shared `rustman-http` TLS settings for `amqps://`,
//! so verification, custom CAs and client certificates behave as for
//! HTTP. Publishes use publisher confirms; consumes read through a
//! temporary exclusive queue unless a named queue is opted into.

mod consume;
mod publish;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use lapin::tcp::{AsyncTcpStream, RustlsConnector};
use lapin::uri::{AMQPScheme, AMQPUri};
use lapin::{Connection, ConnectionProperties};
use rustman_core::model::Operation;
use rustman_core::vars::ResolvedMessaging;

use crate::{MessagingError, MessagingExchange, RunOptions};

pub use consume::{ConsumeMode, consume_mode, delivery_message};
pub use publish::{confirmation_error, properties};

pub(crate) const PROTOCOL: &str = "amqp";

/// Run the publish or consume of a `kind: amqp` request.
pub async fn run(
    resolved: &ResolvedMessaging,
    options: RunOptions,
) -> Result<MessagingExchange, MessagingError> {
    let uri = broker_uri(resolved)?;
    let tls = match uri.scheme {
        AMQPScheme::AMQPS => Some(RustlsConnector::from(Arc::new(crate::tls::client_config(
            resolved,
        )?))),
        AMQPScheme::AMQP => None,
    };
    let connection = connect(uri, tls, options.connect_timeout).await?;
    let mut exchange = MessagingExchange::start(resolved);
    let result = match resolved.operation {
        Operation::Produce => publish::publish_all(&connection, resolved, &mut exchange).await,
        Operation::Consume => consume::consume(&connection, resolved, options, &mut exchange).await,
    };
    let _ = connection.close(200, "bye".into()).await;
    result.map(|()| exchange)
}

/// The broker URI with the request's credentials, which win over any in
/// the URL. Its host is the one the safety policy checked (see
/// [`crate::endpoint::amqp`]).
pub fn broker_uri(resolved: &ResolvedMessaging) -> Result<AMQPUri, MessagingError> {
    let broker = resolved.broker.as_deref().unwrap_or_default();
    let (_, mut uri) = crate::endpoint::amqp(broker)?;
    if let Some(auth) = &resolved.auth {
        uri.authority.userinfo.username = auth.username.clone();
        uri.authority.userinfo.password = auth.password.clone().unwrap_or_default();
    }
    Ok(uri)
}

async fn connect(
    uri: AMQPUri,
    tls: Option<RustlsConnector>,
    timeout: Duration,
) -> Result<Connection, MessagingError> {
    let address = format!("{}:{}", uri.authority.host, uri.authority.port);
    let runtime = lapin::runtime::default_runtime()
        .map_err(|e| MessagingError::broker(PROTOCOL, e.to_string()))?;
    let open = async move |uri: AMQPUri, runtime| {
        let host = uri.authority.host.clone();
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), uri.authority.port))
            .await?
            .collect();
        let stream = AsyncTcpStream::connect(&runtime, addrs).await?;
        match &tls {
            Some(connector) => Ok(stream.into_rustls(connector, &host).await?),
            None => Ok(stream),
        }
    };
    let connecting = Connection::connector(uri, runtime, open, ConnectionProperties::default());
    match tokio::time::timeout(timeout, connecting).await {
        Ok(result) => result.map_err(|e| connect_error(&address, &e.to_string())),
        Err(_) => Err(MessagingError::broker(
            PROTOCOL,
            format!(
                "cannot connect to {address}: timed out after {} ms",
                timeout.as_millis()
            ),
        )),
    }
}

/// The error of a failed connection; a refused login is an
/// authentication failure.
pub fn connect_error(address: &str, message: &str) -> MessagingError {
    if message.contains("ACCESS_REFUSED") || message.contains("authentication") {
        return MessagingError::AuthFailed {
            protocol: PROTOCOL,
            mechanism: "PLAIN",
        };
    }
    MessagingError::broker(PROTOCOL, format!("cannot connect to {address}: {message}"))
}

/// The error of a failed channel operation.
pub(crate) fn broker_error(what: &str, error: &lapin::Error) -> MessagingError {
    MessagingError::broker(PROTOCOL, format!("{what}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_logins_are_authentication_failures() {
        let err = connect_error("h:5672", "ACCESS_REFUSED - Login was refused");
        assert_eq!(err.to_string(), "amqp: authentication failed (PLAIN)");
        let err = connect_error("h:5672", "Connection refused (os error 61)");
        assert_eq!(
            err.to_string(),
            "amqp: cannot connect to h:5672: Connection refused (os error 61)"
        );
    }
}
