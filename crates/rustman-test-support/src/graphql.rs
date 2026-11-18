//! A GraphQL server over a fixed SDL schema. It validates every operation
//! against the schema and answers introspection with `apollo-compiler`'s
//! own executor, so the client's introspection query and its conversion
//! back to SDL are checked against a real implementation.
//!
//! Operations other than introspection get a `no resolvers` error. Every
//! request is recorded.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use apollo_compiler::response::JsonMap;
use apollo_compiler::validation::Valid;
use apollo_compiler::{ExecutableDocument, Schema, introspection};
use bytes::Bytes;
use http::{Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// How the server treats introspection queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Introspection {
    /// Answers every introspection query.
    Full,
    /// Knows only the June 2018 introspection schema: rejects
    /// `specifiedByURL`, as older servers do.
    LegacyOnly,
    /// Rejects introspection, as production servers often do.
    Disabled,
}

/// The error a [`Introspection::Disabled`] server answers with.
pub const INTROSPECTION_DISABLED: &str = "GraphQL introspection is not allowed";

/// One request the server received.
#[derive(Clone, Debug)]
pub struct GraphqlHit {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl GraphqlHit {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Whether this request asked for `__schema`.
    pub fn is_introspection(&self) -> bool {
        self.body.contains("__schema")
    }
}

struct State {
    schema: Valid<Schema>,
    introspection: Introspection,
    hits: Vec<GraphqlHit>,
}

/// A running server; stops when dropped.
pub struct GraphqlServer {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl Drop for GraphqlServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl GraphqlServer {
    /// A server for the schema `sdl` that answers introspection.
    pub async fn start(sdl: &str) -> Self {
        Self::with_introspection(sdl, Introspection::Full).await
    }

    /// A server for the schema `sdl` with the given introspection support.
    pub async fn with_introspection(sdl: &str, introspection: Introspection) -> Self {
        let schema = Schema::parse_and_validate(sdl, "server.graphql").expect("a valid test SDL");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            schema,
            introspection,
            hits: Vec::new(),
        }));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let state = shared.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle(state.clone(), req));
                    let _ = auto::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(tcp), service)
                        .await;
                });
            }
        });
        Self { addr, state, task }
    }

    /// `http://127.0.0.1:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// Every request received so far, in order.
    pub fn hits(&self) -> Vec<GraphqlHit> {
        self.state.lock().unwrap().hits.clone()
    }

    /// How many introspection requests were received.
    pub fn introspection_hits(&self) -> usize {
        self.hits().iter().filter(|h| h.is_introspection()).count()
    }
}

async fn handle(
    state: Arc<Mutex<State>>,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let (parts, body) = req.into_parts();
    let body = body
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let hit = GraphqlHit {
        method: parts.method.to_string(),
        path: parts.uri.path().to_owned(),
        headers: parts
            .headers
            .iter()
            .map(|(n, v)| {
                let value = String::from_utf8_lossy(v.as_bytes()).into_owned();
                (n.as_str().to_owned(), value)
            })
            .collect(),
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    let mut state = state.lock().unwrap();
    let answer = answer(&state, &hit.body);
    state.hits.push(hit);
    Ok(json_response(&answer))
}

fn json_response(value: &Value) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/graphql-response+json")
        .body(Full::new(Bytes::from(value.to_string())))
        .unwrap()
}

fn error(message: &str) -> Value {
    json!({"errors": [{"message": message}]})
}

/// The response to the request body `body`.
fn answer(state: &State, body: &str) -> Value {
    let Ok(request) = serde_json::from_str::<Value>(body) else {
        return error("the body is not JSON");
    };
    let query = request["query"].as_str().unwrap_or_default();
    let operation_name = request["operationName"].as_str();
    match state.introspection {
        Introspection::Disabled if query.contains("__schema") => {
            return error(INTROSPECTION_DISABLED);
        }
        Introspection::LegacyOnly if query.contains("specifiedByURL") => {
            return error("Cannot query field \"specifiedByURL\" on type \"__Type\".");
        }
        _ => {}
    }
    execute(&state.schema, query, operation_name)
}

/// Validate `query` and run its introspection fields.
fn execute(schema: &Valid<Schema>, query: &str, operation_name: Option<&str>) -> Value {
    let document = match ExecutableDocument::parse_and_validate(schema, query, "query.graphql") {
        Ok(document) => document,
        Err(invalid) => {
            let errors: Vec<Value> = invalid
                .errors
                .iter()
                .map(|d| serde_json::to_value(d.to_json()).unwrap())
                .collect();
            return json!({ "errors": errors });
        }
    };
    let Ok(operation) = document.operations.get(operation_name) else {
        return error("unknown operation");
    };
    if !operation
        .root_fields(&document)
        .any(|f| f.name == "__schema")
    {
        return json!({"data": null, "errors": [{"message": "no resolvers"}]});
    }
    let variables = Valid::assume_valid(JsonMap::new());
    match introspection::partial_execute(
        schema,
        &schema.implementers_map(),
        &document,
        operation,
        &variables,
    ) {
        Ok(response) => serde_json::to_value(response).unwrap(),
        Err(e) => error(&e.message().to_string()),
    }
}
