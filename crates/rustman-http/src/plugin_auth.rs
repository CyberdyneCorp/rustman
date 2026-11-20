//! `auth: {type: plugin, scheme: <plugin>/<scheme>, ...}` (plugins spec,
//! "Auth scheme extension point"): the request is signed by a plugin once
//! it is fully built, so the plugin sees the exact method, URL, headers and
//! body that are sent. The plugin may set headers, add query parameters
//! and change the path or query; a different method, host or body is
//! refused, and so is a plugin fault (the request is then not sent).

use std::sync::Arc;

use bytes::Bytes;
use http::header::{HeaderName, HeaderValue};
use http::{Request, Uri};
use http_body_util::{BodyExt, Full};
use rustman_core::model::PluginAuth;
use rustman_core::plugins::{Extensions, SignOutput, SignRequest, split_qualified};

use crate::error::RequestError;

/// The loaded plugins a request may be signed with.
#[derive(Clone)]
pub struct PluginHooks(pub Arc<dyn Extensions>);

impl std::fmt::Debug for PluginHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PluginHooks")
    }
}

impl PartialEq for PluginHooks {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for PluginHooks {}

/// Sign `request` with the scheme of `auth`.
pub(crate) async fn sign(
    request: &mut Request<Full<Bytes>>,
    auth: &PluginAuth,
    plugins: Option<&PluginHooks>,
) -> Result<(), RequestError> {
    let Some(plugins) = plugins else {
        return Err(RequestError::Auth(format!(
            "auth scheme {}: workspace plugins are not loaded",
            auth.scheme
        )));
    };
    let body = request
        .body()
        .clone()
        .collect()
        .await
        .map(http_body_util::Collected::to_bytes)
        .unwrap_or_default();
    let input = SignRequest {
        method: request.method().to_string(),
        url: request.uri().to_string(),
        headers: crate::engine::header_pairs(request.headers()),
        body: body.to_vec(),
        params: auth.params.clone(),
    };
    let output = plugins
        .0
        .sign(&auth.scheme, &input)
        .map_err(RequestError::Auth)?;
    let plugin = split_qualified(&auth.scheme).map_or("", |(plugin, _)| plugin);
    apply(request, plugin, output)
}

/// Add what the plugin returned to `request`.
fn apply(
    request: &mut Request<Full<Bytes>>,
    plugin: &str,
    output: SignOutput,
) -> Result<(), RequestError> {
    if let Some(url) = &output.url {
        let next: Uri = url
            .parse()
            .map_err(|e| invalid(plugin, &format!("returned an invalid URL: {e}")))?;
        if next.scheme() != request.uri().scheme() || next.authority() != request.uri().authority()
        {
            return Err(forbidden(plugin));
        }
        *request.uri_mut() = next;
    }
    for (name, value) in &output.set_headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| invalid(plugin, &format!("returned an invalid header name '{name}'")))?;
        if FRAMING_HEADERS.contains(&name.as_str()) {
            return Err(forbidden(plugin));
        }
        let value = HeaderValue::from_str(value)
            .map_err(|_| invalid(plugin, &format!("returned an invalid value for '{name}'")))?;
        request.headers_mut().insert(name, value);
    }
    for (name, value) in &output.add_query {
        append_query(request, plugin, name, value)?;
    }
    Ok(())
}

fn append_query(
    request: &mut Request<Full<Bytes>>,
    plugin: &str,
    name: &str,
    value: &str,
) -> Result<(), RequestError> {
    let uri = request.uri().to_string();
    let pair = form_urlencoded::Serializer::new(String::new())
        .append_pair(name, value)
        .finish();
    let separator = match uri.split_once('?') {
        None => "?",
        Some((_, "")) => "",
        Some(_) => "&",
    };
    *request.uri_mut() = format!("{uri}{separator}{pair}")
        .parse()
        .map_err(|e| invalid(plugin, &format!("returned an invalid query parameter: {e}")))?;
    Ok(())
}

/// Headers that pick the host (virtual hosting) or frame the body: an auth
/// plugin setting one would change what the spec forbids it to change.
pub(crate) const FRAMING_HEADERS: [&str; 9] = [
    "host",
    "content-length",
    "transfer-encoding",
    "content-encoding",
    "connection",
    "upgrade",
    "te",
    "trailer",
    "expect",
];

fn forbidden(plugin: &str) -> RequestError {
    RequestError::Auth(format!(
        "plugin {plugin}: auth plugins may not change method, host or body"
    ))
}

fn invalid(plugin: &str, message: &str) -> RequestError {
    RequestError::Auth(format!("plugin {plugin}: {message}"))
}

#[cfg(test)]
mod tests {
    use rustman_core::plugins::{PluginExchange, PluginVerdict};

    use super::*;

    /// An auth plugin returning a fixed output, and checking its input.
    struct Fixed(Result<SignOutput, String>);

    impl Extensions for Fixed {
        fn generate(&self, _: &str, _: &str, _: &[String]) -> Result<String, String> {
            unreachable!()
        }
        fn sign(&self, scheme: &str, request: &SignRequest) -> Result<SignOutput, String> {
            assert_eq!(scheme, "hmac/v1");
            assert_eq!(request.method, "POST");
            assert_eq!(request.url, "https://api.example.com/orders?x=1");
            assert_eq!(request.body, b"{\"a\":1}");
            assert_eq!(request.params, [("key_var".into(), "hmac_key".into())]);
            self.0.clone()
        }
        fn check(&self, _: &str, _: &PluginExchange, _: &str) -> PluginVerdict {
            unreachable!()
        }
    }

    fn request() -> Request<Full<Bytes>> {
        Request::post("https://api.example.com/orders?x=1")
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from_static(b"{\"a\":1}")))
            .unwrap()
    }

    async fn signed(output: Result<SignOutput, String>) -> Result<Request<Full<Bytes>>, String> {
        let hooks = PluginHooks(Arc::new(Fixed(output)));
        let auth = PluginAuth {
            scheme: "hmac/v1".into(),
            params: vec![("key_var".into(), "hmac_key".into())],
        };
        let mut request = request();
        sign(&mut request, &auth, Some(&hooks))
            .await
            .map_err(|e| e.to_string())?;
        Ok(request)
    }

    #[tokio::test]
    async fn headers_and_query_are_added() {
        let output = SignOutput {
            set_headers: vec![("X-Signature".into(), "abc".into())],
            add_query: vec![("sig".into(), "a b".into())],
            url: Some("https://api.example.com/orders?x=1&v=2".into()),
        };
        let request = signed(Ok(output)).await.unwrap();
        assert_eq!(request.headers()["x-signature"], "abc");
        assert_eq!(
            request.uri().to_string(),
            "https://api.example.com/orders?x=1&v=2&sig=a+b"
        );
    }

    #[tokio::test]
    async fn the_host_cannot_change() {
        let output = SignOutput {
            url: Some("https://evil.example.net/orders".into()),
            ..SignOutput::default()
        };
        assert_eq!(
            signed(Ok(output)).await.unwrap_err(),
            "plugin hmac: auth plugins may not change method, host or body"
        );
    }

    /// `Host` would move the request to another virtual host, and the
    /// framing headers would change the body: all are refused.
    #[tokio::test]
    async fn host_and_framing_headers_cannot_be_set() {
        for name in ["Host", "content-length", "Transfer-Encoding", "Connection"] {
            let output = SignOutput {
                set_headers: vec![(name.into(), "evil.example.net".into())],
                ..SignOutput::default()
            };
            assert_eq!(
                signed(Ok(output)).await.unwrap_err(),
                "plugin hmac: auth plugins may not change method, host or body",
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn a_plugin_fault_stops_the_request() {
        let error = signed(Err("plugin hmac trapped: wasm trap: unreachable".into())).await;
        assert_eq!(
            error.unwrap_err(),
            "plugin hmac trapped: wasm trap: unreachable"
        );
    }

    #[tokio::test]
    async fn without_plugins_nothing_is_sent() {
        let auth = PluginAuth {
            scheme: "hmac/v1".into(),
            params: Vec::new(),
        };
        let error = sign(&mut request(), &auth, None).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "auth scheme hmac/v1: workspace plugins are not loaded"
        );
    }
}
