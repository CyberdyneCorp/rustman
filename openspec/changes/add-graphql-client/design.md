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
`eventsource-stream` over the `reqwest` byte stream from `rustman-http` (rejected: `reqwest-eventsource` — it owns retries/reconnects, which we don't want for subscriptions that must end on `complete`).

### D5. Upload encoding
`{ "$file": path }` markers are walked in the variables tree, replaced with `null`, and their JSON paths collected into `map` — the spec's own algorithm. Files stream from disk (no full read into memory) using reqwest's multipart streaming parts.

### D6. Subscription result model
A subscription produces an `Exchange` (from `request-history`) whose response is an ordered list of `{ts, payload}` events. Headless bounds (`max_events`, `duration`) live on the request file and can be overridden by `--max-events/--duration` flags. History keeps the first 100 events to bound file size.

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
