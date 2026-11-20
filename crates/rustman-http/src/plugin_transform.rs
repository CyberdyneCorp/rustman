//! `transform: {request: [...]}` (plugins spec, "Transformer extension
//! point"): the request, once built and before auth, goes through each
//! plugin transformer in order. A transformer returns the headers and the
//! body to send; it cannot pick the host or the body framing, which the
//! host sets from the result. A transformer failure stops the request.

use bytes::Bytes;
use http::Request;
use http::header::{CONTENT_LENGTH, HOST, HeaderMap, HeaderName, HeaderValue};
use http_body_util::{BodyExt, Full};
use rustman_core::plugins::PluginMessage;

use crate::PluginHooks;
use crate::error::RequestError;
use crate::plugin_auth::FRAMING_HEADERS;

/// Run `names` over `request`, in order.
pub(crate) async fn transform_request(
    request: &mut Request<Full<Bytes>>,
    names: &[String],
    plugins: Option<&PluginHooks>,
) -> Result<(), RequestError> {
    let Some(plugins) = plugins else {
        return Err(RequestError::Invalid(format!(
            "transformer {}: workspace plugins are not loaded",
            names.join(", ")
        )));
    };
    let body = request
        .body()
        .clone()
        .collect()
        .await
        .map(http_body_util::Collected::to_bytes)
        .unwrap_or_default();
    let url = request.uri().to_string();
    let mut message = PluginMessage {
        headers: crate::engine::header_pairs(request.headers()),
        body: body.to_vec(),
    };
    for name in names {
        message = plugins
            .0
            .transform_request(name, &url, &message)
            .map_err(RequestError::Invalid)?;
    }
    apply(request, message)
}

/// Send the headers and body of `message`, keeping the request's `Host`
/// and setting its `Content-Length`.
fn apply(request: &mut Request<Full<Bytes>>, message: PluginMessage) -> Result<(), RequestError> {
    let mut headers = HeaderMap::new();
    for (name, value) in &message.headers {
        let invalid =
            || RequestError::Invalid(format!("a transformer returned the header '{name}'"));
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| invalid())?;
        if FRAMING_HEADERS.contains(&name.as_str()) {
            continue;
        }
        let value = HeaderValue::from_str(value).map_err(|_| invalid())?;
        headers.append(name, value);
    }
    if let Some(host) = request.headers().get(HOST) {
        headers.insert(HOST, host.clone());
    }
    let framed = request.headers().contains_key(CONTENT_LENGTH);
    if framed || !message.body.is_empty() {
        headers.insert(CONTENT_LENGTH, HeaderValue::from(message.body.len()));
    }
    *request.headers_mut() = headers;
    *request.body_mut() = Full::new(Bytes::from(message.body));
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rustman_core::plugins::{
        Extensions, PluginExchange, PluginVerdict, SignOutput, SignRequest,
    };

    use super::*;

    /// Upper-cases the body, and tries to move the request and its framing.
    struct Upper;

    impl Extensions for Upper {
        fn generate(&self, _: &str, _: &str, _: &[String]) -> Result<String, String> {
            unreachable!()
        }
        fn sign(&self, _: &str, _: &SignRequest) -> Result<SignOutput, String> {
            unreachable!()
        }
        fn check(&self, _: &str, _: &PluginExchange, _: &str) -> PluginVerdict {
            unreachable!()
        }
        fn transform_request(
            &self,
            name: &str,
            url: &str,
            message: &PluginMessage,
        ) -> Result<PluginMessage, String> {
            if name == "acme/fail" {
                return Err("plugin acme: cannot encrypt".to_owned());
            }
            assert_eq!(url, "https://api.example.com/orders");
            let mut headers = message.headers.clone();
            headers.push(("Host".into(), "evil.example.net".into()));
            headers.push(("Transfer-Encoding".into(), "chunked".into()));
            headers.push(("X-Transformed".into(), name.into()));
            Ok(PluginMessage {
                headers,
                body: message.body.to_ascii_uppercase(),
            })
        }
    }

    fn request() -> Request<Full<Bytes>> {
        Request::post("https://api.example.com/orders")
            .header("Host", "api.example.com")
            .header("Content-Length", "5")
            .body(Full::new(Bytes::from_static(b"hello")))
            .unwrap()
    }

    async fn body(request: Request<Full<Bytes>>) -> Bytes {
        request.into_body().collect().await.unwrap().to_bytes()
    }

    #[tokio::test]
    async fn transformers_run_in_order_and_keep_host_and_framing() {
        let hooks = PluginHooks(Arc::new(Upper));
        let mut request = request();
        let names = ["acme/a".to_owned(), "acme/b".to_owned()];
        transform_request(&mut request, &names, Some(&hooks))
            .await
            .unwrap();
        let headers = request.headers().clone();
        assert_eq!(headers["host"], "api.example.com");
        assert_eq!(headers["content-length"], "5");
        assert!(!headers.contains_key("transfer-encoding"));
        let applied: Vec<_> = headers.get_all("x-transformed").iter().collect();
        assert_eq!(applied, ["acme/a", "acme/b"]);
        assert_eq!(body(request).await, "HELLO");
    }

    #[tokio::test]
    async fn a_failing_transformer_stops_the_request() {
        let hooks = PluginHooks(Arc::new(Upper));
        let error = transform_request(&mut request(), &["acme/fail".to_owned()], Some(&hooks))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "plugin acme: cannot encrypt");
        let error = transform_request(&mut request(), &["acme/a".to_owned()], None)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "transformer acme/a: workspace plugins are not loaded"
        );
    }
}
