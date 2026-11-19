# Design

## Context

`bootstrap-core-engine` provides the kind registry, variables and redaction, assertions, history and `rustman run`. `http-requests` owns TLS, proxies, auth helpers and streaming response bodies. `graphql-requests` already set the headless-bounds convention for subscriptions (a cap on events plus a duration), and this change follows it. See proposal.md for the motivation.

## Building on existing code

`rustman-jsonrpc` is the reference for a protocol crate. Reuse, don't rebuild:

- **WebSocket transport**: `rustman_http::open_websocket` (`crates/rustman-http/src/websocket.rs`), shared by GraphQL subscriptions and JSON-RPC, already performs the handshake over the engine's `Connector`: DNS, connect timeout, HTTP `CONNECT`/HTTPS/SOCKS5 proxies (always tunnelled), `TlsSettings` (`ca_bundle`, client certificate, `verify_tls`) with ALPN `http/1.1`, headers, auth helpers and cookies through `request_head`, a subprotocol offer and redacted transport errors. D1's hand-made `CONNECT` tunnel plus `client_async_tls` is not needed. `tokio-tungstenite` is already in `[workspace.dependencies]` with only the `handshake` feature; no TLS feature may be enabled.
  - Gaps to close in `websocket.rs` without changing GraphQL or JSON-RPC behavior: the message/frame cap is `SendOptions.body.max_in_memory` (from `[http] max_body_bytes`, default 10 MiB) instead of a caller value, and `handshake_error` collapses a refused upgrade to `WebSocket handshake failed: HTTP <code>`, dropping the reason phrase, headers and body that this spec records.
- **SSE transport**: `HttpEngine::open_stream` (`crates/rustman-http/src/stream.rs`) returns a `StreamedResponse` read chunk by chunk, with the same auth, signing, cookies, TLS and proxies as `send`. GraphQL over SSE uses it (`crates/rustman-graphql/src/subscription/sse.rs`), with a `pub(crate)` incremental `SseParser` (`event`, `data`, comments, per-event size limit) that ignores `id` and `retry`. D2's parser should be that one, moved to `rustman-http` and extended, not a second implementation.
- **Request model and registry**: kinds are a closed enum in `crates/rustman-core/src/model/kind.rs` (`Request::{Http, Graphql, Jsonrpc, Mcp, Opaque}`, `KindRegistry::default`). New kinds need `model/ws.rs` and `model/sse.rs` (`deny_unknown_fields`), enum variants, the writer in `crates/rustman-core/src/format/write/request.rs`, and resolution in `crates/rustman-core/src/vars/` (as `vars/jsonrpc.rs`).
- **Runner**: `rustman-realtime` depends on `rustman-http`, so the runner reaches it through an injected executor, as `RunOptions::jsonrpc` / `JsonrpcExecutor` (`crates/rustman-http/src/runner/jsonrpc.rs`) and `RunOptions::graphql` do. The record part goes next to `crates/rustman-core/src/run/jsonrpc.rs`, and the report part into `crates/rustman-report/src/run.schema.json` and `docs/schemas/report.schema.json`.
- **Assertions**: per-kind modules in `crates/rustman-core/src/assertions/` (`jsonrpc.rs`, `graphql.rs`, `mcp.rs`) on `serde_json_path` and `regex`; `wait_for` matchers call the same evaluators.
- **CLI, TUI, agent**: command groups under `crates/rustman-cli/src/commands/` (`rpc/` is the reference) with exit codes from `crates/rustman-cli/src/exit.rs`; the `RequestPane` registry in `crates/rustman-tui/src/pane.rs` (`jsonrpc_pane.rs`/`jsonrpc_view.rs` show live events); the agent tool template is `rpc_call` in `crates/rustman-mcp/src/server/tools/rpc.rs` over the session's `GovernedSender`, with `[agent].allow_mutations` from `crates/rustman-core/src/agent.rs`.
- **Test servers**: `crates/rustman-test-support` (`graphql_ws.rs`, `graphql_sse.rs`, `proxy.rs`, `certs.rs`) for WebSocket, SSE, proxy and TLS stubs.

## Goals / Non-Goals

**Goals:**
- One session model for WebSocket and SSE, with one timeline, one assertion surface and one history shape.
- Deterministic headless behavior: every session terminates under `rustman run`.
- Scripted request/response over sockets, without a scripting language.

**Non-Goals:**
- Socket.IO, STOMP or other subprotocol semantics. A raw frame view is enough, and plugins can add more later.
- Serving or mocking realtime endpoints.

## Decisions

### D1. WebSocket: `tokio-tungstenite` with `rustls` (rejected: `fastwebsockets`, `async-tungstenite`)
`tokio-tungstenite` is mature, supports custom request headers and subprotocols, and exposes ping/pong and close frames. `fastwebsockets` is faster but lower level, and throughput isn't our bottleneck. Connections are opened with `rustman_http::open_websocket`, which builds the stream through the engine's connector with the shared `TlsSettings` builder and proxy tunnels, so CA, mTLS and proxies behave exactly as in `http-requests` (see "Building on existing code").

### D2. SSE parser on our own HTTP streaming (rejected: `reqwest-eventsource`, `eventsource-client`)
SSE is a text protocol over a normal HTTP response. Implementing the WHATWG parsing algorithm, about 150 lines, over `rustman-http` streaming bodies keeps timings, proxies, auth and redaction uniform, and allows POST streams (LLM APIs). It also avoids pulling in reqwest, which the core design rejected. The parser is a pure state machine that we fuzz.

### D3. Session engine
One tokio task owns each session. It exposes an `mpsc` command channel (send, close, ping) and a `broadcast` timeline channel used by the TUI, CLI printer and runner. Steps run on the same task as a small interpreter. `wait_for` evaluates matchers against new incoming messages only, starting from the moment the step begins, so earlier messages can't satisfy it by accident.

### D4. Matchers reuse the `assertions` engine
`wait_for` matchers and message assertions call the same JSONPath (RFC 9535) and regex evaluators as `assertions`, so there's one syntax and one set of error messages.

### D5. Exchange shape
`protocol: ws|sse`, `handshake {status, headers, subprotocol}`, `timeline[] {dir, t_ms, size, type, payload}`, `end {reason, close_code?, close_reason?}`, `dropped_entries`. Payloads above 64 KiB are stored in history as truncated previews plus a size field, so history stays small. Full payloads remain available in the live session.

### D6. Bounds and limits
The defaults (50 messages / 30 s headless, 16 MiB per message, 10000 timeline entries) are a little more generous than GraphQL's 10 events, because raw feeds are chattier. All of them can be overridden per file and per CLI flag.

## Risks / Trade-offs

- [Servers that require a specific `Origin` header] → `headers` can set `Origin`. It isn't sent by default, which matches non-browser clients.
- [Binary protocols such as protobuf over WebSocket show as unreadable hex] → Hex and base64 views now; `plugins` can add decoders later.
- [Timing in `wait_for` tests is flaky in CI] → Tests use a local tokio server with deterministic sequencing; no sleeps-as-synchronization.
- [Unbounded server push in the TUI] → `max_timeline` caps memory, and the UI virtualizes the list.

## Migration Plan

Additive. There's nothing to migrate.

## Open Questions

- Whether `permessage-deflate` compression should be negotiated by default. Tungstenite's support is limited; this can be added later without changing the specs.
- SSE parser: move GraphQL's `SseParser` into `rustman-http` and extend it with `id`, `retry` and the leading BOM (keeps one parser, as D2 intends), or adopt `sse-stream`, already a workspace dependency of the MCP client (`crates/rustman-mcp/src/client`)?
- Message cap: `limits.max_message_bytes` defaults to 16 MiB, while `open_websocket` caps messages at `[http] max_body_bytes` (10 MiB by default). Should the realtime limit override the HTTP cap, or be bounded by it?
- Bound names: GraphQL and JSON-RPC subscriptions use top-level `max_events` / `max_duration_ms` (default 10 events, 30 s); this spec uses `limits.max_messages` / `limits.duration_ms`. Keep the spec's names, or align them before implementation?
