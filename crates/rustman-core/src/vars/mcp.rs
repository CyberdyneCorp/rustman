//! Resolving a `kind: mcp` request before it is sent: `{{name}}` is
//! interpolated in the string values of `params` (keys and other scalars
//! are kept). The server definition is resolved by the client when it
//! connects. Every missing name is reported at once.

use std::fmt;

use serde_json::{Map, Value};

use super::graphql::object;
use super::resolver::{ResolveError, Resolver, UsedSecret};
use crate::model::{McpOperation, Request};
use crate::redact::Redactor;
use crate::workspace::RequestRef;

/// An mcp request with every placeholder of its params replaced. The
/// `Debug` output is redacted.
#[derive(Clone, PartialEq)]
pub struct ResolvedMcp {
    pub server: String,
    pub operation: McpOperation,
    pub params: Map<String, Value>,
    /// Every secret used in the params.
    pub secrets: Vec<UsedSecret>,
}

impl ResolvedMcp {
    /// The redactor for everything shown or recorded about this request.
    pub fn redactor(&self) -> Redactor {
        let mut redactor = Redactor::new();
        for secret in &self.secrets {
            redactor.add(secret);
        }
        redactor
    }
}

impl fmt::Debug for ResolvedMcp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let raw = format!(
            "ResolvedMcp {{ server: {:?}, operation: {}, params: {:?} }}",
            self.server, self.operation, self.params
        );
        f.write_str(&self.redactor().text(&raw))
    }
}

/// Resolve the mcp request `request` through `resolver`.
pub fn resolve_mcp(
    request: &RequestRef<'_>,
    resolver: &Resolver<'_>,
) -> Result<ResolvedMcp, ResolveError> {
    let Request::Mcp(mcp) = request.request() else {
        let kind = request.request().kind();
        return Err(ResolveError::Unsupported(format!(
            "request kind '{kind}' is not an mcp request"
        )));
    };
    let mut i = resolver.interpolator();
    let params = object(&mut i, &mcp.params());
    let (params, secrets) = i.finish(params)?;
    Ok(ResolvedMcp {
        server: mcp.server.clone(),
        operation: mcp.operation,
        params,
        secrets,
    })
}
