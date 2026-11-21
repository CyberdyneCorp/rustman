//! The rustls client config every protocol connects with: the request's
//! `tls` block and settings through `rustman-http`'s shared TLS settings
//! builder, so verification, custom CAs and client certificates behave as
//! for HTTP.

use rustman_core::vars::ResolvedMessaging;
use rustman_http::TlsSettings;

use crate::MessagingError;

/// The rustls client config for `resolved`, from its `tls` settings.
pub fn client_config(resolved: &ResolvedMessaging) -> Result<rustls::ClientConfig, MessagingError> {
    let connection = &resolved.connection;
    let verify = connection.settings.verify_tls.unwrap_or(true);
    let password = connection.settings.client_pkcs12_password.as_deref();
    let settings = TlsSettings::load(&connection.tls, password, verify)
        .map_err(|e| MessagingError::Tls(e.to_string()))?;
    settings
        .client_config(&[])
        .map_err(|e| MessagingError::Tls(e.to_string()))
}
