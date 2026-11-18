//! The document of a graphql request: inline, or read from `document_file`.
//! It is never interpolated, so it is sent byte for byte as written.

use rustman_core::RequestRef;
use rustman_core::model::{DocumentSource, GraphqlRequest};

use crate::error::GraphqlError;

/// The document of `graphql` (the request `request` points at). A
/// `document_file` is relative to the request file's folder and must stay
/// inside the workspace.
pub fn load_document(
    request: &RequestRef<'_>,
    graphql: &GraphqlRequest,
) -> Result<String, GraphqlError> {
    match graphql.document_source() {
        DocumentSource::Inline(text) => Ok(text.to_owned()),
        DocumentSource::File(raw) => {
            let path = request.contain_file(raw)?;
            std::fs::read_to_string(&path).map_err(|e| GraphqlError::Read {
                path: raw.to_owned(),
                message: e.to_string(),
            })
        }
    }
}
