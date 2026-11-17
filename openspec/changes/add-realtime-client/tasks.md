# Tasks

## 1. Crate and request model

- [ ] 1.1 Create `crates/rustman-realtime` with `tokio-tungstenite` (rustls feature) pinned. Verify that `cargo build -p rustman-realtime` passes on Linux, macOS and Windows CI.
- [ ] 1.2 Register kinds `ws` and `sse` and parse both file formats (url, headers, auth, subprotocols, steps, limits, tls, method, body, last_event_id, reconnect). Verify with unit tests for valid files, the `must use ws:// or wss://` error, and the registered-kind list.
- [ ] 1.3 Interpolate variables in URLs, headers and step strings, and redact secrets in recorded URLs and headers. Verify with a test that history stores `«redacted:api_token»`.

## 2. WebSocket engine

- [ ] 2.1 Handshake with headers, auth helpers and subprotocols, recording the response. Verify with tests for subprotocol negotiation and a `401` handshake failure (exit code 3 in `rustman run`).
- [ ] 2.2 TLS through the shared `TlsSettings`, plus the proxy CONNECT tunnel. Verify with tests for a self-signed rejection, success with a custom CA, and connecting through a local CONNECT proxy.
- [ ] 2.3 Send text, binary (base64 or file) and JSON frames; auto-pong; client pings with RTT; close handshake with a 2000 ms wait. Verify with tests for each frame type, a server-initiated close of 1008, and RTT recorded on pong.
- [ ] 2.4 Steps interpreter (`send`, `wait_for`, `sleep`, `close`) using the `assertions` JSONPath and regex evaluators. Verify with tests for a request/response match and the `wait_for timed out after 300 ms` error.

## 3. SSE engine

- [ ] 3.1 WHATWG event-stream parser as a pure state machine. Verify with unit tests (multi-line data, comments, CRLF and CR line endings, `retry`) plus a `cargo fuzz` target that runs in CI for 60 s.
- [ ] 3.2 SSE sessions over `rustman-http` streaming (GET and POST), with the content-type check. Verify with tests for a POST stream and the `unexpected content type application/json` error.
- [ ] 3.3 Reconnect with `retry`, `Last-Event-ID`, `max_attempts`, and stop on `204`. Verify with tests for resume carrying `Last-Event-ID: 42` and stopping on 204.

## 4. Timeline, bounds, assertions, history

- [ ] 4.1 Timeline recording with direction, relative time, size and type, plus the `max_timeline` cap with a dropped counter. Verify with a test that 10001 entries keep 10000 and record `dropped_entries: 1`.
- [ ] 4.2 Headless bounds and the message size limit (close 1009). Verify with tests for an endless feed stopping at 50 messages and a 2 MiB frame closing with 1009.
- [ ] 4.3 Assertions `messages`, `close_code`, JSONPath over `$.messages`, latency to first message, and captures. Verify with tests for the count/content scenario and the `close_code: expected 1000, got 1011` failure.
- [ ] 4.4 History exchanges with protocol `ws`/`sse`, with payload previews truncated above 64 KiB. Verify with a test inspecting the stored exchange.
- [ ] 4.5 Document the `kind: ws` and `kind: sse` file formats and steps in `docs/request-files.md`. Verify with the docs examples loading in a test.

## 5. CLI, TUI, agent tools

- [ ] 5.1 `rustman ws connect` (stdin frames when not a TTY) and `rustman sse listen`, with `--json`, `--max-messages`, `--duration` and `--header`. Verify with assert_cmd tests for exactly 3 JSON lines and stdin echo.
- [ ] 5.2 TUI realtime pane: connect/close controls, frame composer (text/JSON/binary), timeline with direction/type/text filters, pretty JSON, and a virtualized list. Verify with ratatui `TestBackend` snapshot tests and a test that 10000 entries render without blocking input.
- [ ] 5.3 Conditional agent tool `realtime_listen`, bounded to 100 messages / 60 s and going through the safety policy, with frame sending gated by `allow_mutations`. Verify with tests for the blocked read-only send, a blocked host, and a redacted result.
