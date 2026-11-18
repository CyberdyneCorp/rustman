//! Which schema validates a graphql request. In order of precedence: the
//! request's `schema_file`, the `schema_file` of its `[[graphql.endpoints]]`
//! entry in `rustman.toml`, then the cached introspection result. Choosing
//! a schema never sends a request.

use std::path::PathBuf;

use rustman_core::RequestRef;
use rustman_core::model::{GraphqlRequest, Request};
use rustman_core::settings::GraphqlSettings;

use crate::cache::{SchemaCache, normalize_endpoint};
use crate::error::GraphqlError;
use crate::schema::GraphqlSchema;

/// Where the active schema came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaOrigin {
    /// The request's `schema_file`, as written.
    RequestFile(String),
    /// A `[[graphql.endpoints]]` `schema_file`, as written.
    SettingsFile(String),
    /// The cache file, and when it was fetched.
    Cache { path: PathBuf, fetched_at: String },
}

/// The schema a request is validated against.
#[derive(Clone, Debug)]
pub struct ActiveSchema {
    pub origin: SchemaOrigin,
    pub schema: GraphqlSchema,
}

/// The schema for the graphql request `request`, whose endpoint resolved
/// to `endpoint`, or `None` when no schema is available (only syntax is
/// checked then). A `schema_file` is read even when a cache exists, and
/// the cache is not read at all.
pub fn active_schema(
    request: &RequestRef<'_>,
    settings: &GraphqlSettings,
    endpoint: &str,
    cache: &SchemaCache,
) -> Result<Option<ActiveSchema>, GraphqlError> {
    let Request::Graphql(graphql) = request.request() else {
        return Ok(None);
    };
    if let Some(raw) = &graphql.schema_file {
        let path = request.contain_file(raw)?;
        let schema = GraphqlSchema::from_file(&path, raw)?;
        return Ok(Some(ActiveSchema {
            origin: SchemaOrigin::RequestFile(raw.clone()),
            schema,
        }));
    }
    if let Some(raw) = settings_schema_file(settings, graphql, endpoint) {
        let path = request.contain_workspace_file(raw)?;
        let schema = GraphqlSchema::from_file(&path, raw)?;
        return Ok(Some(ActiveSchema {
            origin: SchemaOrigin::SettingsFile(raw.to_owned()),
            schema,
        }));
    }
    Ok(cache.load(endpoint)?.map(|cached| ActiveSchema {
        origin: SchemaOrigin::Cache {
            path: cached.path,
            fetched_at: cached.fetched_at,
        },
        schema: cached.schema,
    }))
}

/// The `schema_file` of the `[[graphql.endpoints]]` entry whose `url` is
/// the request's endpoint as written, or the same URL once normalized.
fn settings_schema_file<'s>(
    settings: &'s GraphqlSettings,
    graphql: &GraphqlRequest,
    endpoint: &str,
) -> Option<&'s str> {
    let resolved = normalize_endpoint(endpoint).ok();
    settings
        .endpoints
        .iter()
        .find(|entry| {
            entry.url == graphql.endpoint
                || (resolved.is_some() && normalize_endpoint(&entry.url).ok() == resolved)
        })
        .and_then(|entry| entry.schema_file.as_deref())
}
