//! Loading the schema of a GraphQL tab off the render loop: the active
//! schema (`schema_file`, then the cache) when the tab opens, or a fresh
//! introspection on the refresh action. Like a send, the job resolves the
//! request in the active environment, so errors are redacted before they
//! reach the screen.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use rustman_core::model::Request;
use rustman_core::secrets::SecretBackend;
use rustman_core::vars::{ResolvedGraphql, Resolver, resolve_graphql};
use rustman_core::workspace::RequestRef;
use rustman_core::{Redactor, Workspace};
use rustman_graphql::{
    GraphqlError, SchemaCache, SchemaOrigin, active_schema, load_document, refresh_schema,
};
use rustman_http::{HttpEngine, SendOptions};

use crate::app::Action;
use crate::graphql_pane::{LoadedSchema, SchemaState};
use crate::send::{loose_entry, secret_store, target, with_edit};
use crate::tabs::TabId;

/// What a schema load needs, owned, to run on another thread.
#[derive(Clone)]
pub struct SchemaJob {
    pub tab: TabId,
    /// Introspect the endpoint (and replace the cache) instead of reading
    /// what is there.
    pub refresh: bool,
    pub request: Request,
    /// The tab's file, relative to the workspace root.
    pub path: Option<PathBuf>,
    /// `None` in a scratch session: nothing is cached there.
    pub workspace: Option<Box<Workspace>>,
    pub environment: Option<String>,
    pub backend: Option<SecretBackend>,
}

impl fmt::Debug for SchemaJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SchemaJob")
            .field("tab", &self.tab)
            .field("refresh", &self.refresh)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl PartialEq for SchemaJob {
    fn eq(&self, other: &Self) -> bool {
        (self.tab, self.refresh, &self.path) == (other.tab, other.refresh, &other.path)
    }
}

impl Eq for SchemaJob {}

/// What a load found.
#[derive(Debug)]
pub struct SchemaLoaded {
    pub state: SchemaState,
    /// The text of `document_file`, when the request has one.
    pub document: Option<String>,
}

/// Run `job`. Blocks: the effect runner calls it on a blocking thread.
pub fn execute(job: &SchemaJob) -> Action {
    let result = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(load(job)),
        Err(e) => Err(format!("cannot start the async runtime: {e}")),
    };
    Action::GraphqlSchema {
        tab: job.tab,
        refresh: job.refresh,
        result: result.map(Box::new),
    }
}

async fn load(job: &SchemaJob) -> Result<SchemaLoaded, String> {
    let Some(ws) = &job.workspace else {
        if job.refresh {
            return Err("Scratch session: open a workspace to cache schemas".to_owned());
        }
        return Ok(SchemaLoaded {
            state: SchemaState::Ready(None),
            document: None,
        });
    };
    let (ws, path) = with_edit(ws, job.path.as_deref(), &job.request);
    let loose = loose_entry(job.path.as_deref(), &job.request);
    let request = target(&ws, path.as_deref(), &loose);
    let resolved = resolve(job, &ws, &request)?;
    let redactor = resolved.redactor();
    let document = document_text(&request, &redactor);
    let cache = SchemaCache::for_workspace(&ws.root);
    if job.refresh {
        fetch(&ws, &resolved, &cache)
            .await
            .map_err(|e| redacted(&redactor, &e))?;
    }
    let endpoint = &resolved.http.url.text;
    let active = active_schema(&request, &ws.settings.graphql, endpoint, &cache)
        .map_err(|e| redacted(&redactor, &e))?;
    let loaded = active.map(|active| {
        let overview = active.schema.overview();
        LoadedSchema {
            origin: origin_label(&active.origin),
            schema: Arc::new(active.schema),
            overview,
        }
    });
    Ok(SchemaLoaded {
        state: SchemaState::Ready(loaded),
        document,
    })
}

/// The request resolved in the job's environment.
fn resolve(
    job: &SchemaJob,
    ws: &Workspace,
    request: &RequestRef<'_>,
) -> Result<ResolvedGraphql, String> {
    let store = secret_store(&ws.root, job.backend)?;
    let env = ws
        .environments
        .iter()
        .find(|e| job.environment.as_deref() == Some(e.name.as_str()));
    let resolver = Resolver::for_request(&ws.settings, request, env, &store);
    resolve_graphql(request, &resolver).map_err(|e| e.to_string())
}

/// The text of `document_file`, when the request has one and it reads.
fn document_text(request: &RequestRef<'_>, redactor: &Redactor) -> Option<String> {
    let Request::Graphql(graphql) = request.request() else {
        return None;
    };
    graphql.document_file.as_ref()?;
    Some(match load_document(request, graphql) {
        Ok(text) => text,
        Err(e) => format!("# {}", redacted(redactor, &e)),
    })
}

async fn fetch(
    ws: &Workspace,
    resolved: &ResolvedGraphql,
    cache: &SchemaCache,
) -> Result<(), GraphqlError> {
    let options = SendOptions::for_request(&ws.settings, &ws.root, &resolved.http)
        .map_err(|e| GraphqlError::IntrospectionFailed(e.to_string()))?;
    let limit = ws.settings.graphql.max_schema_bytes;
    refresh_schema(&HttpEngine::new(), &options, resolved, cache, limit).await?;
    Ok(())
}

fn redacted(redactor: &Redactor, error: &GraphqlError) -> String {
    redactor.text(&error.to_string()).into_owned()
}

/// How the schema line names where the schema came from.
fn origin_label(origin: &SchemaOrigin) -> String {
    match origin {
        SchemaOrigin::RequestFile(file) => format!("schema_file {file}"),
        SchemaOrigin::SettingsFile(file) => format!("rustman.toml schema_file {file}"),
        SchemaOrigin::Cache { fetched_at, .. } => format!("cache, fetched {fetched_at}"),
    }
}
