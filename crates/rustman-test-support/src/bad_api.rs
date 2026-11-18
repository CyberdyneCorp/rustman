//! `bad-api`: a deliberately broken API and its OpenAPI document, the
//! fixture of the exploration tests. Each route breaks one documented
//! promise, so each detector has something to find.
//!
//! | Route | Bug |
//! |-------|-----|
//! | `GET /users/{id}` | the body has no `email`, which the schema requires |
//! | `POST /users` | a missing `name` answers `500` with a Java stack trace; a wrong-typed `age` is accepted |
//! | `PUT /users` | undocumented, yet answers `200` |
//! | `PUT /users/{id}` | answers `200` but changes nothing |
//! | `DELETE /users/{id}` | answers `204` but deletes nothing |
//! | `GET /admin/users` | requires a bearer token, yet answers `200` without one, listing [`TOKEN_A`] |
//! | `GET /orders/{orderId}` | any valid token reads any order (BOLA) |
//! | `GET /search` | a short `q` answers `500` with SQL error text and the request headers |
//! | `GET /status` | answers `202` (only `200` is documented) with `*` CORS and credentials |
//! | every route | no `X-Content-Type-Options` header |
//!
//! `GET /reports/{reportId}` has no producer in the document. Valid tokens
//! are [`TOKEN_A`] and [`TOKEN_B`]; one user (id 1) exists from the start.
//!
//! [`BadApi::start_fixed`] serves the same API with every bug above fixed,
//! so regression request files exported from findings can be shown to fail
//! against the broken API and pass against the fixed one.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http::{HeaderMap, Method, Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Identity A's bearer token.
pub const TOKEN_A: &str = "token-alice-7f3a9c";
/// Identity B's bearer token.
pub const TOKEN_B: &str = "token-bob-2b8e41";

/// The OpenAPI 3.1 document of `bad-api`.
pub const OPENAPI: &str = include_str!("bad_api.yaml");

type Body = Full<Bytes>;

/// One request the server received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub method: String,
    /// The path and query.
    pub target: String,
}

#[derive(Default)]
struct State {
    /// Serve the fixed API.
    fixed: bool,
    hits: Vec<Hit>,
    users: BTreeMap<u64, Value>,
    next_user: u64,
    /// Orders by id, with their owner's token.
    orders: BTreeMap<String, (String, Value)>,
    /// Requests with this method, on a path starting with this prefix,
    /// are never answered.
    stall: Option<(String, String)>,
}

/// A running `bad-api`; stops when dropped.
pub struct BadApi {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl Drop for BadApi {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl BadApi {
    pub async fn start() -> Self {
        Self::serve(false, None).await
    }

    /// The same API with its bugs fixed.
    pub async fn start_fixed() -> Self {
        Self::serve(true, None).await
    }

    /// The broken API, except that `method` requests on paths starting
    /// with `prefix` are received (and listed in [`BadApi::hits`]) but
    /// never answered.
    pub async fn start_stalling(method: &str, prefix: &str) -> Self {
        Self::serve(false, Some((method.to_owned(), prefix.to_owned()))).await
    }

    async fn serve(fixed: bool, stall: Option<(String, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut state = State {
            fixed,
            next_user: 2,
            stall,
            ..State::default()
        };
        state.users.insert(
            1,
            json!({"id": 1, "name": "Ana", "email": "ana@example.com"}),
        );
        let state = Arc::new(Mutex::new(state));
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
        format!("http://127.0.0.1:{}{path}", self.addr.port())
    }

    /// Every request received so far, in order.
    pub fn hits(&self) -> Vec<Hit> {
        self.state.lock().unwrap().hits.clone()
    }

    /// The ids of the users that exist now.
    pub fn user_ids(&self) -> Vec<u64> {
        self.state.lock().unwrap().users.keys().copied().collect()
    }
}

async fn handle(
    state: Arc<Mutex<State>>,
    req: Request<Incoming>,
) -> Result<Response<Body>, Infallible> {
    let (parts, body) = req.into_parts();
    let body = body
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let target = parts
        .uri
        .path_and_query()
        .map_or("/", |p| p.as_str())
        .to_owned();
    let path = parts.uri.path().to_owned();
    let stalled = {
        let mut state = state.lock().unwrap();
        state.hits.push(Hit {
            method: parts.method.to_string(),
            target,
        });
        state.stall.as_ref().is_some_and(|(m, prefix)| {
            parts.method == m.as_str() && path.starts_with(prefix.as_str())
        })
    };
    if stalled {
        std::future::pending::<()>().await;
    }
    let mut state = state.lock().unwrap();
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let call = Call {
        method: &parts.method,
        headers: &parts.headers,
        query: parts.uri.query().unwrap_or_default(),
        body: &body,
    };
    if !state.fixed {
        return Ok(route(&mut state, &call, &segments));
    }
    let mut response = fixed_route(&mut state, &call, &segments)
        .unwrap_or_else(|| route(&mut state, &call, &segments));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

/// The fixed API's answer where it differs from the broken one.
fn fixed_route(state: &mut State, call: &Call<'_>, segments: &[&str]) -> Option<Response<Body>> {
    let body = call.json();
    let field = |name: &str| body.as_ref().and_then(|b| b.get(name));
    let short_query = call.param("q").is_some_and(|q| q.chars().count() < 3);
    Some(match (call.method.as_str(), segments) {
        ("GET", ["admin", "users"]) if call.token().is_none() => unauthorized(),
        ("PUT", ["users"]) => reply(
            StatusCode::METHOD_NOT_ALLOWED,
            &json!({"error": "method not allowed"}),
        ),
        ("POST", ["users"]) if body.is_some() && field("name").is_none() => {
            bad_request("name is required")
        }
        ("POST", ["users"]) if field("age").is_some_and(|a| !a.is_i64()) => {
            bad_request("age must be an integer")
        }
        ("GET" | "PUT" | "DELETE", ["users", id]) => fixed_user(state, call, id)?,
        ("GET", ["orders", id]) => fixed_order(state, call, id)?,
        ("GET", ["search"]) if call.token().is_some() && short_query => {
            bad_request("q must have at least 3 characters")
        }
        ("GET", ["status"]) => reply(StatusCode::OK, &json!({"status": "ok"})),
        _ => return None,
    })
}

/// Users with their email, updates that stick and deletes that delete.
fn fixed_user(state: &mut State, call: &Call<'_>, id: &str) -> Option<Response<Body>> {
    let id = id.parse::<u64>().ok()?;
    let user = state.users.get_mut(&id)?;
    match call.method.as_str() {
        "GET" => Some(reply(StatusCode::OK, user)),
        "PUT" => {
            let body = call.json()?;
            let wrong_age = body.get("age").is_some_and(|a| !a.is_i64());
            if wrong_age || invalid_user(&body).is_some() || !body.contains_key("name") {
                return Some(bad_request("invalid user"));
            }
            user["name"] = body["name"].clone();
            Some(reply(StatusCode::OK, user))
        }
        _ => {
            state.users.remove(&id);
            Some(
                Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    }
}

/// Orders only for their owner.
fn fixed_order(state: &State, call: &Call<'_>, id: &str) -> Option<Response<Body>> {
    let (owner, _) = state.orders.get(id)?;
    let token = call.token()?;
    (owner != token).then(not_found)
}

struct Call<'a> {
    method: &'a Method,
    headers: &'a HeaderMap,
    query: &'a str,
    body: &'a [u8],
}

impl Call<'_> {
    fn token(&self) -> Option<&str> {
        let value = self.headers.get(header::AUTHORIZATION)?.to_str().ok()?;
        let token = value.strip_prefix("Bearer ")?;
        [TOKEN_A, TOKEN_B].into_iter().find(|t| *t == token)
    }

    fn json(&self) -> Option<serde_json::Map<String, Value>> {
        serde_json::from_slice::<Value>(self.body)
            .ok()?
            .as_object()
            .cloned()
    }

    fn param(&self, name: &str) -> Option<String> {
        form_urlencoded::parse(self.query.as_bytes())
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    }
}

fn route(state: &mut State, call: &Call<'_>, segments: &[&str]) -> Response<Body> {
    let method = call.method.as_str();
    match (method, segments) {
        ("GET", ["users"]) => reply(
            StatusCode::OK,
            &Value::Array(state.users.values().cloned().collect()),
        ),
        ("POST", ["users"]) => create_user(state, call),
        ("PUT", ["users"]) => reply(StatusCode::OK, &json!({"updated": 0})),
        (_, ["users", id]) => user(state, call, id),
        ("GET", ["admin", "users"]) => reply(
            StatusCode::OK,
            &json!([{"id": 1, "role": "admin", "api_token": TOKEN_A}]),
        ),
        ("POST", ["orders"]) => create_order(state, call),
        ("GET", ["orders", id]) => order(state, call, id),
        ("GET", ["search"]) => search(call),
        ("GET", ["reports", _]) => not_found(),
        ("GET", ["status"]) => status(),
        (_, ["users" | "orders" | "search" | "status"] | ["admin", "users"] | ["reports", _]) => {
            reply(
                StatusCode::METHOD_NOT_ALLOWED,
                &json!({"error": "method not allowed"}),
            )
        }
        _ => not_found(),
    }
}

fn reply(status: StatusCode, body: &Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap()
}

fn text(status: StatusCode, body: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}

fn bad_request(message: &str) -> Response<Body> {
    reply(StatusCode::BAD_REQUEST, &json!({"error": message}))
}

fn not_found() -> Response<Body> {
    reply(StatusCode::NOT_FOUND, &json!({"error": "not found"}))
}

fn unauthorized() -> Response<Body> {
    reply(StatusCode::UNAUTHORIZED, &json!({"error": "unauthorized"}))
}

/// A user body's problem; `age` is not checked for its type (a bug).
fn invalid_user(body: &serde_json::Map<String, Value>) -> Option<&'static str> {
    let name = body.get("name")?;
    match name.as_str() {
        None => return Some("name must be a string"),
        Some(n) if n.is_empty() || n.chars().count() > 50 => {
            return Some("name must have 1 to 50 characters");
        }
        Some(_) => {}
    }
    if body.get("email").is_some_and(|e| !e.is_string()) {
        return Some("email must be a string");
    }
    let age = body.get("age").and_then(Value::as_i64);
    age.filter(|a| !(0..=150).contains(a))
        .map(|_| "age must be 0 to 150")
}

fn create_user(state: &mut State, call: &Call<'_>) -> Response<Body> {
    let Some(body) = call.json() else {
        return bad_request("expected a JSON object");
    };
    if !body.contains_key("name") {
        let trace = "java.lang.NullPointerException: name\n\tat com.example.UserService.create(UserService.java:42)\n\tat com.example.UserController.post(UserController.java:17)\n";
        return text(StatusCode::INTERNAL_SERVER_ERROR, trace.to_owned());
    }
    if let Some(problem) = invalid_user(&body) {
        return bad_request(problem);
    }
    let id = state.next_user;
    state.next_user += 1;
    let email = body
        .get("email")
        .cloned()
        .unwrap_or_else(|| json!("new@example.com"));
    let user = json!({"id": id, "name": body["name"], "email": email});
    state.users.insert(id, user.clone());
    reply(StatusCode::CREATED, &user)
}

fn user(state: &mut State, call: &Call<'_>, id: &str) -> Response<Body> {
    let Some(user) = id.parse::<u64>().ok().and_then(|id| state.users.get(&id)) else {
        return match call.method.as_str() {
            "GET" | "PUT" | "DELETE" => not_found(),
            _ => reply(
                StatusCode::METHOD_NOT_ALLOWED,
                &json!({"error": "method not allowed"}),
            ),
        };
    };
    match call.method.as_str() {
        "GET" => {
            let mut shown = user.clone();
            shown.as_object_mut().unwrap().remove("email");
            reply(StatusCode::OK, &shown)
        }
        "PUT" => match call
            .json()
            .map(|b| (invalid_user(&b), b.contains_key("name")))
        {
            Some((None, true)) => reply(StatusCode::OK, user),
            Some((Some(problem), _)) => bad_request(problem),
            _ => bad_request("name is required"),
        },
        "DELETE" => Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Full::new(Bytes::new()))
            .unwrap(),
        _ => reply(
            StatusCode::METHOD_NOT_ALLOWED,
            &json!({"error": "method not allowed"}),
        ),
    }
}

fn create_order(state: &mut State, call: &Call<'_>) -> Response<Body> {
    let Some(owner) = call.token() else {
        return unauthorized();
    };
    let Some(body) = call.json() else {
        return bad_request("expected a JSON object");
    };
    let item = body
        .get("item")
        .and_then(Value::as_str)
        .filter(|i| !i.is_empty());
    let quantity = body
        .get("quantity")
        .and_then(Value::as_i64)
        .filter(|q| (1..=100).contains(q));
    let (Some(item), Some(quantity)) = (item, quantity) else {
        return bad_request("item and quantity (1 to 100) are required");
    };
    let id = format!("o-{}", state.orders.len() + 1);
    let order = json!({"id": id, "item": item, "quantity": quantity});
    state.orders.insert(id, (owner.to_owned(), order.clone()));
    reply(StatusCode::CREATED, &order)
}

/// Any valid token reads any order: the owner is never checked.
fn order(state: &State, call: &Call<'_>, id: &str) -> Response<Body> {
    if call.token().is_none() {
        return unauthorized();
    }
    match state.orders.get(id) {
        Some((_owner, order)) => reply(StatusCode::OK, order),
        None => not_found(),
    }
}

/// A short query breaks the (pretend) SQL, and the error page dumps the
/// request headers.
fn search(call: &Call<'_>) -> Response<Body> {
    if call.token().is_none() {
        return unauthorized();
    }
    let Some(q) = call.param("q") else {
        return bad_request("q is required");
    };
    if q.chars().count() < 3 {
        let headers: Vec<String> = call
            .headers
            .iter()
            .map(|(n, v)| format!("{n}: {}", String::from_utf8_lossy(v.as_bytes())))
            .collect();
        let page = format!(
            "SQLSTATE[42000]: You have an error in your SQL syntax near '{q}'\nrequest headers:\n{}\n",
            headers.join("\n")
        );
        return text(StatusCode::INTERNAL_SERVER_ERROR, page);
    }
    reply(StatusCode::OK, &json!({"results": []}))
}

fn status() -> Response<Body> {
    Response::builder()
        .status(StatusCode::ACCEPTED)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::ACCESS_CONTROL_ALLOW_CREDENTIALS, "true")
        .body(Full::new(Bytes::from(json!({"status": "ok"}).to_string())))
        .unwrap()
}
