# Tasks

See design.md "Building on existing code" for the components each task reuses.

## 1. Crate and request model

- [x] 1.1 Create `crates/rustman-realtime` depending on `rustman-core` and `rustman-http`, using the workspace `tokio-tungstenite` (feature `handshake` only; TLS comes from `rustman-http`) and adding it to `[workspace.dependencies]` as `rustman-jsonrpc` is. Verify that `cargo build -p rustman-realtime` passes on Linux, macOS and Windows CI.
- [x] 1.2 Add `model/ws.rs` and `model/sse.rs` in `rustman-core` (url, headers, auth, subprotocols, steps, limits, tls, method, body, last_event_id, reconnect), the `Request::Ws`/`Request::Sse` variants, `KindRegistry::default` entries, and the round-trip writer in `format/write/request.rs`. Verify with unit tests for valid files, the `must use ws:// or wss://` error, a write/load round trip, and the registered-kind list in the unknown-kind error.
- [x] 1.3 Interpolate variables in URLs, headers and step strings (a `vars/realtime.rs` resolver next to `vars/jsonrpc.rs`), and redact secrets in recorded URLs and headers with `Redactor::for_request`. Verify with a test that history stores `«redacted:api_token»`.

## 2. WebSocket engine

- [x] 2.1 Connect through `rustman_http::open_websocket` with headers, auth helpers and subprotocols, recording the handshake response. Extend `WebSocketError::Handshake` so a refused upgrade keeps status, reason and body (up to 64 KiB) without changing the GraphQL/JSON-RPC messages. Verify with tests for subprotocol negotiation and a `401` handshake failure (`ws: handshake failed: 401 Unauthorized`, exit code 3 in `rustman run`).
- [x] 2.2 TLS and proxies come from `open_websocket` (`TlsSettings`, `CONNECT`/SOCKS5 tunnels): add only realtime-level tests. Verify with tests for a self-signed rejection naming the host, success with a custom CA, and connecting through the `rustman-test-support` proxy.
- [x] 2.3 Send text, binary (base64 or file) and JSON frames; auto-pong; client pings with RTT; close handshake with a 2000 ms wait. Verify with tests for each frame type, a server-initiated close of 1008, and RTT recorded on pong.
- [x] 2.4 Steps interpreter (`send`, `wait_for`, `sleep`, `close`) using the JSONPath (`serde_json_path`) and regex evaluators of `rustman_core::assertions`. Verify with tests for a request/response match and the `wait_for timed out after 300 ms` error.

## 3. SSE engine

- [x] 3.1 Move the `SseParser` of `crates/rustman-graphql/src/subscription/sse.rs` into `rustman-http` (design D7) and complete the WHATWG algorithm (`id`, `retry`, leading BOM), switching GraphQL over SSE to it. Verify with unit tests (multi-line data, comments, CRLF and CR line endings, `retry`, `id`), the existing GraphQL SSE tests unchanged, plus a `cargo fuzz` target that runs in CI for 60 s.
- [x] 3.2 SSE sessions over `HttpEngine::open_stream` (GET and POST), with the content-type check. Verify with tests for a POST stream and the `unexpected content type application/json` error.
- [x] 3.3 Reconnect with `retry`, `Last-Event-ID`, `max_attempts`, and stop on `204`. Verify with tests for resume carrying `Last-Event-ID: 42` and stopping on 204.

## 4. Timeline, bounds, assertions, history

- [x] 4.1 Timeline recording with direction, relative time, size and type, plus the `max_timeline` cap with a dropped counter. Verify with a test that 10001 entries keep 10000 and record `dropped_entries: 1`.
- [x] 4.2 Headless bounds and the message size limit (close 1009; `open_websocket` must take the cap from the caller, see design open questions). Verify with tests for an endless feed stopping at 50 messages and a 2 MiB frame closing with 1009.
- [x] 4.3 Assertions `messages`, `close_code`, JSONPath over `$.messages`, latency to first message, and captures, as a `realtime` module beside `assertions/jsonrpc.rs`. Verify with tests for the count/content scenario and the `close_code: expected 1000, got 1011` failure.
- [x] 4.4 Runner and history: a `RunOptions::realtime` executor following `JsonrpcExecutor` (`crates/rustman-http/src/runner/jsonrpc.rs`), a record part next to `rustman-core/src/run/jsonrpc.rs` with protocol `ws`/`sse`, payload previews truncated above 64 KiB, and the `realtime` exchange in `run.schema.json` and `docs/schemas/report.schema.json`. Verify with a test inspecting the stored exchange and a run report validated against `rustman report schema`.
- [x] 4.5 Document the `kind: ws` and `kind: sse` file formats and steps in `docs/request-files.md` (and a protocol page like `docs/jsonrpc.md`). Verify with the docs examples loading in a test.

## 5. CLI, TUI, agent tools

- [x] 5.1 `rustman ws connect` (stdin frames when not a TTY) and `rustman sse listen` as command groups beside `commands/rpc/`, with `--json`, `--max-messages`, `--duration` and `--header`, mapping errors through `exit.rs`, and completions. Verify with assert_cmd tests for exactly 3 JSON lines and stdin echo.
- [x] 5.2 TUI realtime pane registered in `Registry::builtin` (`crates/rustman-tui/src/pane.rs`): connect/close controls, frame composer (text/JSON/binary), timeline with direction/type/text filters, pretty JSON, and a virtualized list; live entries are redacted before they reach the TUI, as for JSON-RPC subscriptions. Verify with ratatui `TestBackend` snapshot tests and a test that 10000 entries render without blocking input.
- [x] 5.3 Conditional agent tool `realtime_listen` in `crates/rustman-mcp/src/server/tools/` (template: `rpc.rs`), bounded to 100 messages / 60 s, checking the session's safety policy origin allowlist, with frame sending gated by `[agent].allow_mutations`. Verify with tests for the blocked read-only send, a blocked host, and a redacted result.

## 6. Hardening

- [x] 6.1 Cap SSE `event` and `id` values at 4 KiB (longer ones are ignored) and set the last event id only when an event is dispatched (WHATWG). Count types and ids in the timeline's 256 MiB budget, and cut recorded types and ids to 1 KiB after redaction. Verify with parser tests for the cap and for an unfinished event not moving `Last-Event-ID`, a timeline budget test and a record preview test.
- [x] 6.2 Read `ws connect` stdin through a bounded command queue (64 frames), and redact body errors of streamed responses with the response's credentials. Verify with a test that a fast stdin waits for the session and a test of the streamed response's redactor.
- [x] 6.3 Verify the TUI's JSON pretty-print of payloads, the 50-message default of `rustman run` for a request without `limits`, and that plain CLI output prints multi-line payloads as received while `--json` keeps one line per message.
- [x] 6.4 After a 1009 close, read and discard the rest of what the server sends until it closes the connection (at most 2000 ms) instead of dropping a socket with unread bytes, whose reset made Windows servers lose the close frame. Verify with a test that a server sending 16 MiB completes its send and receives the 1009 close.
