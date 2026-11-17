# Design

## Context

`bootstrap-core-engine` provides the kind registry, variables and redaction, assertions, history and `rustman run`. `http-requests` owns TLS, proxies, auth helpers and streaming response bodies. `graphql-requests` already set the headless-bounds convention for subscriptions (a cap on events plus a duration), and this change follows it. See proposal.md for the motivation.

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
`tokio-tungstenite` is mature, supports custom request headers, subprotocols and a rustls connector, and exposes ping/pong and close frames. `fastwebsockets` is faster but lower level, and throughput isn't our bottleneck. We pass in our own `rustls::ClientConfig` from the shared `TlsSettings` builder, so CA and mTLS behave exactly as in `http-requests`. For proxies, we open an HTTP CONNECT tunnel through `rustman-http` and hand the stream to `client_async_tls`.

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
