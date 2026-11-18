# Design

## Context

`bootstrap-core-engine` provides the workspace loader (with `kind: graphql` reserved), variable resolution, the `rustman-http` transport (TLS, proxies, auth helpers, timings), assertions, history and the `rustman run` runner. `add-terminal-ui` provides the pane framework, editor widget and keymap. This change adds a `rustman-graphql` crate that plugs into those seams; it owns no transport of its own for HTTP operations.

## Goals / Non-Goals

**Goals:**
- Schema-aware editing (validation, completion, explorer) that works offline once a schema is cached.
- One execution path for TUI, CLI and the future agent interface (`add-agent-mcp-server`).
- Subscriptions that are testable headlessly (bounded by events/duration).

**Non-Goals:**
- A GraphQL language server over LSP (the engine API is internal; exposing LSP can come later).
- Incremental delivery (`@defer`/`@stream`) parsing beyond showing raw multipart chunks.

## Decisions

### D1. Parser/validator: `apollo-compiler` (rejected: `async-graphql-parser`, `graphql-parser`)
`apollo-compiler` implements the full spec validation rule set with source locations, builds a typed schema from SDL or introspection, and is maintained by Apollo for the router. `async-graphql-parser` only parses (validation lives inside the async-graphql server crate and assumes server-side resolvers); `graphql-parser` is unmaintained-ish and parse-only. Completion is implemented on top of the compiler's typed schema plus a cursor-to-node walk over the parsed document (tolerant parsing: apollo-parser keeps a CST with errors, which completion needs mid-typing).

### D2. Introspection → SDL
Introspection JSON is converted to an `apollo_compiler::Schema` via a small converter (introspection result → SDL text → schema). The cache stores the raw introspection JSON (lossless) plus a computed SDL hash so `--out schema.graphql` and diffing are cheap. Cache key = SHA-256 of the normalized endpoint URL (scheme, host, port, path; query stripped) so environment-specific hosts don't collide.

### D3. Transport reuse
HTTP queries/mutations/uploads/APQ/SSE go through `rustman-http`, so TLS, proxies, cookies, auth helpers, timings and redaction come for free. WebSocket subscriptions use `tokio-tungstenite` with rustls, reusing the TLS config builder exported by `rustman-http` (rejected: `graphql-ws-client` crate — adds its own abstractions over the protocol and lags on legacy-protocol fallback; the protocol is small enough to implement directly with a state machine and is easier to test with a fake server).

### D4. SSE client
A small incremental `text/event-stream` parser in `rustman-graphql` over the response body chunks (`hyper::body::Incoming`) that `rustman-http` hands back from `HttpEngine::open_stream`, so SSE inherits its TLS, proxies, auth, cookies and redaction. GraphQL over SSE only needs the `event` and `data` fields and comments, which fit in a few lines with tests for every chunk split (rejected: `eventsource-stream`, which pulls in `nom` for the same subset; `reqwest-eventsource` — it requires reqwest, which `bootstrap-core-engine` D2 rejected, and it owns retries/reconnects, which we don't want for subscriptions that must end on `complete`).

### D5. Upload encoding
`{ "$file": path }` markers are walked in the variables tree, replaced with `null`, and their JSON paths collected into `map` — the spec's own algorithm. The body is built by the `multipart/form-data` encoder `rustman-http` already uses for `body.multipart`: the `operations` and `map` parts first, then one part per file, read from disk when the request is sent (rejected: reqwest's multipart API, for the same reason as D4). Streaming each file with a `tokio_util::io::ReaderStream` was the first plan, but the `rustman-http` transport sends a buffered body (`Full<Bytes>`) so that redirects (307/308) and Digest retries can replay it; a streaming body would change that shared transport for every request kind, so it is left to a follow-up change that streams `body.file` and `body.multipart` alike. Until then an upload holds its files in memory while it is sent, as `kind: http` multipart bodies do.

### D6. Subscription result model
A subscription produces an `Exchange` (from `request-history`) whose response is an ordered list of `{ts, payload}` events. Headless bounds (`max_events`, `duration`) live on the request file and can be overridden by `--max-events/--duration` flags. History keeps the first 100 events to bound file size.

### D6b. Runner integration
`rustman-http` owns the runner but cannot depend on `rustman-graphql` (which depends on it). The runner resolves graphql requests, learns their secrets, evaluates assertions and captures and records history as for HTTP ones, and delegates sending to a `GraphqlExecutor` trait object passed in `RunOptions::graphql`; `rustman-graphql` implements it (`GraphqlRunner`) and the CLI wires it. A subscription's exchange carries the handshake (or stream) status and headers with the collected result `{"data": [...], "errors": [...]}` as its body, so every assertion kind works unchanged (rejected: moving the runner into a crate above both, a large move that every other stream would have to rebase over).

### D7. Assertions integration
GraphQL assertions are registered as extra assertion kinds through the assertion registry defined in `bootstrap-core-engine`; `data` JSONPath checks reuse the existing JSONPath evaluator with the root shifted to `$.data`.

## Risks / Trade-offs

- [apollo-compiler API churn between majors] → pin a minor version; isolate behind a `schema` module with our own types for explorer/completion data.
- [Legacy WS detection is heuristic (subprotocol negotiation)] → explicit `ws_protocol` override; integration tests against fake servers for both protocols.
- [Huge schemas (10k+ types) slow the TUI] → build the schema once per cache load on a background task; completion queries are O(fields of one type).
- [Introspection disabled in production] → `schema_file` path and clear error hint.

## Migration Plan

Additive. Request files gain optional `kind: graphql` fields under schema `version: 1`; older Rustman builds already report the kind as unsupported (per `bootstrap-core-engine`).

## Open Questions

- Should the schema cache be shareable via Git (opt-in `graphql/schemas/` in the workspace instead of `.rustman/`)? Deferrable: affects only the cache location setting.
