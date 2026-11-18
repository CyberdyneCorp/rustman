//! Picking the operation a request sends, and the checks that depend only
//! on its type (no schema needed).

use apollo_compiler::ast::{self, Definition, OperationType};
use rustman_core::model::{GraphqlMethod, GraphqlTransport};
use serde::Serialize;

use crate::error::GraphqlError;

/// The file name documents are parsed under; diagnostics carry positions,
/// not this name.
pub(crate) const DOCUMENT_PATH: &str = "document.graphql";

/// Query, mutation or subscription.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationKind {
    Query,
    Mutation,
    Subscription,
}

impl OperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Mutation => "mutation",
            Self::Subscription => "subscription",
        }
    }

    fn of(operation_type: OperationType) -> Self {
        match operation_type {
            OperationType::Query => Self::Query,
            OperationType::Mutation => Self::Mutation,
            OperationType::Subscription => Self::Subscription,
        }
    }
}

/// The operation a request sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedOperation {
    /// The operation's own name; `None` for an anonymous operation.
    pub name: Option<String>,
    pub kind: OperationKind,
}

/// The operation of `document` that `operation_name` names. Without a
/// name, the document must hold exactly one operation. A document with
/// syntax errors is read as far as it parses.
pub fn select_operation(
    document: &str,
    operation_name: Option<&str>,
) -> Result<SelectedOperation, GraphqlError> {
    let ast = match ast::Document::parse(document, DOCUMENT_PATH) {
        Ok(ast) => ast,
        Err(with_errors) => with_errors.partial,
    };
    let operations: Vec<&ast::OperationDefinition> = ast
        .definitions
        .iter()
        .filter_map(|d| match d {
            Definition::OperationDefinition(op) => Some(op.as_ref()),
            _ => None,
        })
        .collect();
    let chosen = match (operation_name, operations.as_slice()) {
        (_, []) => return Err(GraphqlError::NoOperation),
        (Some(name), _) => operations
            .iter()
            .find(|op| op.name.as_ref().is_some_and(|n| n == name))
            .ok_or_else(|| GraphqlError::UnknownOperation(name.to_owned()))?,
        (None, [only]) => only,
        (None, many) => return Err(GraphqlError::AmbiguousOperation(many.len())),
    };
    Ok(SelectedOperation {
        name: chosen.name.as_ref().map(|n| n.to_string()),
        kind: OperationKind::of(chosen.operation_type),
    })
}

/// Reject an operation the HTTP transport cannot send with `method`:
/// mutations over GET, and subscriptions over plain HTTP.
pub fn check_http_method(
    operation: &SelectedOperation,
    transport: GraphqlTransport,
    method: GraphqlMethod,
) -> Result<(), GraphqlError> {
    if transport != GraphqlTransport::Http {
        return Ok(());
    }
    match (operation.kind, method) {
        (OperationKind::Mutation, GraphqlMethod::Get) => Err(GraphqlError::MutationOverGet),
        (OperationKind::Subscription, _) => Err(GraphqlError::SubscriptionOverHttp),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_operation_is_selected_without_a_name() {
        for (document, name) in [("{ a }", None), ("query Users { a }", Some("Users"))] {
            let op = select_operation(document, None).unwrap();
            assert_eq!(op.name.as_deref(), name);
            assert_eq!(op.kind, OperationKind::Query);
        }
    }

    #[test]
    fn several_operations_need_a_name() {
        let document = "query A { a }\nmutation B { b }\nfragment F on Query { a }";
        let err = select_operation(document, None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "graphql: document has 2 operations; set operation_name"
        );
        let b = select_operation(document, Some("B")).unwrap();
        assert_eq!(b.kind, OperationKind::Mutation);
        assert_eq!(b.name.as_deref(), Some("B"));
        let err = select_operation(document, Some("C")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "graphql: operation 'C' is not in the document"
        );
    }

    #[test]
    fn a_document_without_operations_is_rejected() {
        let err = select_operation("fragment F on Query { a }", None).unwrap_err();
        assert_eq!(err, GraphqlError::NoOperation);
    }

    #[test]
    fn a_partly_parsed_document_still_selects() {
        let op = select_operation("subscription S { a(", None).unwrap();
        assert_eq!(op.kind, OperationKind::Subscription);
    }

    #[test]
    fn mutations_cannot_use_get() {
        let mutation = select_operation("mutation { a }", None).unwrap();
        let query = select_operation("{ a }", None).unwrap();
        let http = GraphqlTransport::Http;
        let err = check_http_method(&mutation, http, GraphqlMethod::Get).unwrap_err();
        assert_eq!(err.to_string(), "graphql: mutations cannot use GET");
        assert!(check_http_method(&mutation, http, GraphqlMethod::Post).is_ok());
        assert!(check_http_method(&query, http, GraphqlMethod::Get).is_ok());
    }

    #[test]
    fn subscriptions_need_a_streaming_transport() {
        let sub = select_operation("subscription { a }", None).unwrap();
        let post = GraphqlMethod::Post;
        let err = check_http_method(&sub, GraphqlTransport::Http, post).unwrap_err();
        assert!(err.to_string().contains("use transport ws or sse"), "{err}");
        assert!(check_http_method(&sub, GraphqlTransport::Ws, post).is_ok());
        assert!(check_http_method(&sub, GraphqlTransport::Sse, post).is_ok());
    }
}
