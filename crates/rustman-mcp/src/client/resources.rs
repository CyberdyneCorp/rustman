//! Resources (mcp-client spec "Resources"): the paginated lists of
//! resources and templates, `resources/read` with text or base64 blobs,
//! URI-template expansion, and subscriptions whose
//! `notifications/resources/updated` land in the notifications stream.

use std::collections::BTreeMap;

use rmcp::model::{
    ClientRequest, ListResourceTemplatesRequest, ListResourcesRequest, ReadResourceRequest,
    ReadResourceRequestParams, Resource, ResourceTemplate, ServerResult, SubscribeRequest,
    SubscribeRequestParams, UnsubscribeRequest, UnsubscribeRequestParams,
};
use serde::Serialize;
use serde_json::Value;

use super::error::ClientError;
use super::info::Operation;
use super::notify::redact_value;
use super::request::{Exchange, RequestOptions};
use super::session::McpSession;

/// One item of a `resources/read` answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResourceContent {
    pub uri: String,
    pub mime_type: Option<String>,
    pub text: Option<String>,
    /// Base64, as sent.
    pub blob: Option<String>,
}

impl ResourceContent {
    fn from_value(value: &Value) -> Self {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        Self {
            uri: text("uri").unwrap_or_default(),
            mime_type: text("mimeType"),
            text: text("text"),
            blob: text("blob"),
        }
    }
}

impl McpSession {
    /// `resources/list`, every page.
    pub async fn list_resources(
        &self,
        options: &RequestOptions,
    ) -> Result<Exchange<Vec<Resource>>, ClientError> {
        self.require(Operation::ListResources)?;
        self.sender()
            .paginate(
                "resources/list",
                options,
                |p| ClientRequest::ListResourcesRequest(ListResourcesRequest::with_param(p)),
                |result| match result {
                    ServerResult::ListResourcesResult(page) => {
                        Some((page.resources, page.next_cursor))
                    }
                    _ => None,
                },
            )
            .await
    }

    /// `resources/templates/list`, every page.
    pub async fn list_resource_templates(
        &self,
        options: &RequestOptions,
    ) -> Result<Exchange<Vec<ResourceTemplate>>, ClientError> {
        self.require(Operation::ListResourceTemplates)?;
        self.sender()
            .paginate(
                "resources/templates/list",
                options,
                |p| {
                    ClientRequest::ListResourceTemplatesRequest(
                        ListResourceTemplatesRequest::with_param(p),
                    )
                },
                |result| match result {
                    ServerResult::ListResourceTemplatesResult(page) => {
                        Some((page.resource_templates, page.next_cursor))
                    }
                    _ => None,
                },
            )
            .await
    }

    /// `resources/read` of `uri`.
    pub async fn read_resource(
        &self,
        uri: &str,
        options: &RequestOptions,
    ) -> Result<Exchange<Vec<ResourceContent>>, ClientError> {
        self.require(Operation::ReadResource)?;
        let params = ReadResourceRequestParams::new(uri);
        let request = ClientRequest::ReadResourceRequest(ReadResourceRequest::new(params));
        let sender = self.sender();
        let exchange = sender.send(request, options).await?;
        let ServerResult::ReadResourceResult(result) = &exchange.result else {
            return Err(sender.unexpected("resources/read"));
        };
        let contents = result
            .contents
            .iter()
            .filter_map(|c| serde_json::to_value(c).ok())
            .map(|v| ResourceContent::from_value(&redact_value(&self.redactor(), v)))
            .collect();
        Ok(exchange.map(|_| contents))
    }

    /// `resources/subscribe` to `uri`; its updates are offered for a re-read.
    pub async fn subscribe(&self, uri: &str, options: &RequestOptions) -> Result<(), ClientError> {
        self.require(Operation::Subscribe)?;
        let request = ClientRequest::SubscribeRequest(SubscribeRequest::new(
            SubscribeRequestParams::new(uri),
        ));
        self.state().subscribe(uri);
        if let Err(err) = self.sender().send(request, options).await {
            self.state().unsubscribe(uri);
            return Err(err);
        }
        Ok(())
    }

    /// `resources/unsubscribe` from `uri`.
    pub async fn unsubscribe(
        &self,
        uri: &str,
        options: &RequestOptions,
    ) -> Result<(), ClientError> {
        self.require(Operation::Subscribe)?;
        let params = UnsubscribeRequestParams::new(uri);
        let request = ClientRequest::UnsubscribeRequest(UnsubscribeRequest::new(params));
        self.sender().send(request, options).await?;
        self.state().unsubscribe(uri);
        Ok(())
    }
}

/// The variable names of a URI template, in order.
pub fn template_variables(template: &str) -> Vec<String> {
    expressions(template)
        .flat_map(|(_, names)| names)
        .map(str::to_owned)
        .collect()
}

/// Expand an RFC 6570 URI template (simple `{var}` and reserved `{+var}`
/// expressions) with `params`. Every variable is required.
pub fn expand_template(
    template: &str,
    params: &BTreeMap<String, String>,
) -> Result<String, ClientError> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let end = rest[start..]
            .find('}')
            .map(|e| start + e)
            .ok_or_else(|| ClientError::Invalid(format!("unclosed expression in {template}")))?;
        out.push_str(&rest[..start]);
        out.push_str(&expression(&rest[start + 1..end], params)?);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn expression(body: &str, params: &BTreeMap<String, String>) -> Result<String, ClientError> {
    let (reserved, names) = match body.strip_prefix('+') {
        Some(names) => (true, names),
        None => (false, body),
    };
    let values = names
        .split(',')
        .map(|name| {
            let value = params.get(name.trim()).ok_or_else(|| {
                ClientError::Invalid(format!("missing template parameter {}", name.trim()))
            })?;
            Ok(encode(value, reserved))
        })
        .collect::<Result<Vec<_>, ClientError>>()?;
    Ok(values.join(","))
}

fn expressions(template: &str) -> impl Iterator<Item = (bool, Vec<&str>)> {
    template.split('{').skip(1).filter_map(|part| {
        let body = &part[..part.find('}')?];
        let (reserved, names) = match body.strip_prefix('+') {
            Some(names) => (true, names),
            None => (false, body),
        };
        Some((reserved, names.split(',').map(str::trim).collect()))
    })
}

/// Percent-encode `value`: everything but unreserved characters, and with
/// `reserved` also keeping the reserved set.
fn encode(value: &str, reserved: bool) -> String {
    const RESERVED: &str = ":/?#[]@!$&'()*+,;=%";
    let mut out = String::new();
    for c in value.chars() {
        let keep =
            c.is_ascii_alphanumeric() || "-._~".contains(c) || (reserved && RESERVED.contains(c));
        if keep {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for byte in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn simple_and_reserved_expansion() {
        let p = params(&[("path", "README.md"), ("dir", "docs/a b")]);
        assert_eq!(
            expand_template("file:///{path}", &p).unwrap(),
            "file:///README.md"
        );
        assert_eq!(
            expand_template("file:///{dir}", &p).unwrap(),
            "file:///docs%2Fa%20b"
        );
        assert_eq!(
            expand_template("file:///{+dir}", &p).unwrap(),
            "file:///docs/a%20b"
        );
        assert_eq!(
            expand_template("x://{path,dir}", &p).unwrap(),
            "x://README.md,docs%2Fa%20b"
        );
    }

    #[test]
    fn missing_parameters_and_bad_templates_fail() {
        let err = expand_template("file:///{path}", &params(&[])).unwrap_err();
        assert_eq!(err.to_string(), "mcp: missing template parameter path");
        assert!(expand_template("file:///{path", &params(&[])).is_err());
    }

    #[test]
    fn variables_are_listed() {
        assert_eq!(
            template_variables("db://{+schema}/{table,id}"),
            ["schema", "table", "id"]
        );
        assert!(template_variables("plain").is_empty());
    }
}
