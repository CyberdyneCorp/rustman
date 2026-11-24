# Proposal

## Why

Many APIs push data instead of answering requests: chat, trading feeds, notifications, LLM token streams and dashboards use WebSocket or Server-Sent Events. GraphQL subscriptions are already covered by `graphql-requests`, but raw WebSocket and SSE endpoints still need wscat, websocat or a browser console, with no saved sessions, no assertions and no CI. This change makes them first-class in the same workspace, runner and TUI.

## What Changes

- Register request kinds `ws` and `sse` in the `workspace` kind registry.
- WebSocket sessions: connect with headers, subprotocols and auth from `environments`; send text, binary and JSON frames; ping/pong; close with codes and reasons.
- Scripted sequences for WebSocket: an ordered list of `send`, `wait_for` (match by JSONPath or regex, with a timeout), `sleep` and `close` steps, so request files can drive request/response protocols over a socket.
- SSE sessions: connect, parse `event`/`data`/`id`/`retry`, resume with `Last-Event-ID`, and a reconnect policy.
- A message timeline (timestamp, direction, size, type) with filtering in the TUI.
- Headless bounds (max messages, duration) so sessions terminate under `rustman run`.
- Message size limits, history recording, and assertions and captures on received messages.
- CLI: `rustman ws connect` and `rustman sse listen`, with JSON-lines output for piping.
- A TUI realtime pane registered through the `tui` protocol pane extension point.
- Conditional, bounded, read-only agent tools.

## Capabilities

### New Capabilities

- `realtime-sessions`: defining, running and inspecting WebSocket and Server-Sent Events sessions from saved request files, the CLI and the TUI, including scripted WebSocket sequences, SSE resume and reconnect, headless bounds, assertions and history.

### Modified Capabilities

None. This builds on `workspace`, `environments`, `http-requests` (TLS, proxies, auth helpers), `assertions`, `cli-runner`, `request-history`, `tui` and `agent-mcp-interface` without changing their requirements.

## Out of Scope

- GraphQL over WebSocket or SSE (owned by `graphql-requests`).
- Socket.IO, STOMP, MQTT-over-WebSocket and other protocols layered on WebSocket (possible later plugins via `plugins`).
- WebTransport and HTTP/3.
- Serving WebSocket or SSE endpoints (mocking).

## Impact

- New crate `crates/rustman-realtime` on top of `rustman-http`'s shared WebSocket helper (`open_websocket`, tokio-tungstenite framing over the engine's TLS and proxy connector) and streaming bodies (`open_stream`) with the SSE parser shared with GraphQL over SSE.
- `rustman-core` gains the `ws`/`sse` request models, kinds, resolution, assertions and record parts.
- `rustman-cli` gains the `ws` and `sse` subcommands; `rustman-tui` gains the realtime pane.
- No new system libraries.
