# Spec Delta

## Purpose

Lets users open, script, inspect and test WebSocket and Server-Sent Events sessions from saved request files, the CLI and the TUI, with a message timeline, bounded headless runs, assertions on received messages and history.

## ADDED Requirements

### Requirement: Kind registration
This change SHALL register request kinds `ws` and `sse` in the `workspace` kind registry and a realtime pane for both through the `tui` protocol pane extension point.

#### Scenario: Kinds listed
- **GIVEN** a build that includes this change
- **WHEN** a request file has an unknown kind
- **THEN** the error's list of registered kinds SHALL include `ws` and `sse`

### Requirement: WebSocket request file
A `kind: ws` file SHALL accept `url` (`ws://` or `wss://`), `headers`, `auth`, `subprotocols` (ordered list), `steps` (scripted sequence), `limits` and `tls`. `{{variable}}` placeholders SHALL resolve in the URL, headers and string values of steps. An `http(s)://` URL SHALL be rejected with `ws: url must use ws:// or wss://`.

#### Scenario: Valid file
- **GIVEN** `kind: ws`, `url: "wss://{{host}}/feed"`, `subprotocols: [v2.feed]`
- **WHEN** the workspace loads
- **THEN** the request SHALL load as a WebSocket session

#### Scenario: Wrong scheme
- **GIVEN** `kind: ws` with `url: https://example.com/feed`
- **WHEN** the workspace loads
- **THEN** loading SHALL fail with `ws: url must use ws:// or wss://`

### Requirement: WebSocket handshake
Connecting SHALL perform the RFC 6455 opening handshake with the configured headers, auth helpers from `http-requests`, and `Sec-WebSocket-Protocol` from `subprotocols`. The handshake response status, headers and the negotiated subprotocol SHALL be recorded. A non-101 response SHALL fail with `ws: handshake failed: <status>` and include the response body (up to 64 KiB).

#### Scenario: Subprotocol negotiated
- **GIVEN** `subprotocols: [v2.feed, v1.feed]` and a server that accepts `v1.feed`
- **WHEN** the session connects
- **THEN** the exchange SHALL record negotiated subprotocol `v1.feed`

#### Scenario: Handshake rejected
- **GIVEN** a server that answers the upgrade with `401 Unauthorized`
- **WHEN** the session connects
- **THEN** it SHALL fail with `ws: handshake failed: 401 Unauthorized` and the run SHALL exit with code 3

### Requirement: WebSocket TLS
`wss://` connections SHALL use TLS with verification on, honoring the same CA, client certificate and `insecure` settings as `http-requests`. Proxies from `http-requests` settings SHALL be used through HTTP CONNECT.

#### Scenario: Self-signed certificate
- **GIVEN** a `wss://` server with a self-signed certificate and no custom CA
- **WHEN** the session connects
- **THEN** it SHALL fail with a TLS verification error naming the host

### Requirement: Sending frames
Users and steps SHALL be able to send text frames, binary frames (given as base64 or `{file: path}`) and JSON frames (a YAML/JSON value serialized compactly as a text frame). Sending SHALL fail with `ws: not connected` when the socket is closed.

#### Scenario: JSON frame
- **GIVEN** a step `send: {json: {op: subscribe, channel: "ticker"}}`
- **WHEN** the step runs
- **THEN** a text frame `{"op":"subscribe","channel":"ticker"}` SHALL be sent

#### Scenario: Binary frame from file
- **GIVEN** a step `send: {binary: {file: ./fixtures/blob.bin}}`
- **WHEN** the step runs
- **THEN** one binary frame with the file's bytes SHALL be sent

### Requirement: Scripted sequences
`steps` SHALL run in order: `send`, `wait_for` (`json: {path, equals|matches|exists}` or `regex`, with `timeout_ms`, default 5000), `sleep` (ms) and `close` (code, reason). A `wait_for` that times out SHALL fail the session with `ws: wait_for timed out after <n> ms (<matcher>)`. Messages received while no `wait_for` is active SHALL still be recorded.

#### Scenario: Request and response over a socket
- **GIVEN** steps `send {json: {id: 1, op: ping}}` then `wait_for {json: {path: "$.id", equals: 1}}`
- **WHEN** the server replies `{"id":1,"op":"pong"}` after 50 ms
- **THEN** the wait SHALL succeed and the session SHALL continue to the next step

#### Scenario: Wait timeout
- **GIVEN** `wait_for {regex: "pong", timeout_ms: 300}` and a server that never replies
- **WHEN** the step runs
- **THEN** the session SHALL fail with `ws: wait_for timed out after 300 ms (regex pong)`

### Requirement: Ping, pong and close
The client SHALL answer server pings automatically and MAY send pings (`send: {ping: <base64>}`), recording each pong with its round-trip time. Closing SHALL send a close frame with code (default 1000) and reason, wait up to 2000 ms for the server's close, then drop the connection. The close code and reason from either side SHALL be recorded.

#### Scenario: Server closes
- **GIVEN** an open session
- **WHEN** the server sends close code 1008 with reason `policy`
- **THEN** the exchange SHALL record `closed by server: 1008 policy`

### Requirement: SSE request file
A `kind: sse` file SHALL accept `url` (`http(s)://`), `method` (default `GET`), `headers`, `auth`, `body` (for POST-based streams such as LLM APIs), `last_event_id`, `reconnect` and `limits`. The request SHALL send `Accept: text/event-stream` unless overridden.

#### Scenario: POST stream
- **GIVEN** `kind: sse`, `method: POST`, a JSON body and a server that streams `text/event-stream`
- **WHEN** the session runs
- **THEN** the events SHALL be parsed from the POST response stream

### Requirement: SSE parsing
The system SHALL parse the event stream per the WHATWG HTML specification: `event`, `data` (multi-line joined with `\n`), `id`, `retry` fields, comment lines ignored, and an event dispatched on each blank line. Each event SHALL be recorded with its type (default `message`), id, data and receive time. A response whose content type is not `text/event-stream` SHALL fail with `sse: unexpected content type <type>`.

#### Scenario: Multi-line data
- **GIVEN** the stream `event: update\ndata: line1\ndata: line2\nid: 7\n\n`
- **WHEN** it is parsed
- **THEN** one event SHALL be recorded with type `update`, id `7` and data `line1\nline2`

#### Scenario: Wrong content type
- **GIVEN** a server returning `200` with `Content-Type: application/json`
- **WHEN** the SSE session starts
- **THEN** it SHALL fail with `sse: unexpected content type application/json`

### Requirement: SSE field limits and last event id
An `id` SHALL become the last event id only when its event is dispatched, never while the event is still being read. An `event` or `id` value longer than 4 KiB SHALL be ignored, as if the line were absent.

#### Scenario: Oversized event type or id
- **GIVEN** the stream `id: 7\n\n` followed by an event whose `event` and `id` values are each 5000 bytes long, with `data: a`
- **WHEN** it is parsed
- **THEN** the event SHALL be recorded with type `message`, id `7` and data `a`

#### Scenario: Id of an unfinished event
- **GIVEN** a stream that delivered event id `41`, then sent `id: 42\ndata: x\n` and dropped before the blank line
- **WHEN** the client reconnects
- **THEN** the reconnect request SHALL carry `Last-Event-ID: 41`

### Requirement: SSE reconnect and resume
When the stream ends or the connection drops, the client SHALL reconnect after the last `retry` value (default 3000 ms), sending `Last-Event-ID` with the last received id, up to `reconnect.max_attempts` (default 5, `0` disables). `last_event_id` in the file SHALL set the initial header. A `204 No Content` response SHALL stop reconnecting.

#### Scenario: Resume after drop
- **GIVEN** a stream that delivered event id `42` and then dropped
- **WHEN** the client reconnects
- **THEN** the reconnect request SHALL carry `Last-Event-ID: 42`

#### Scenario: Server asks to stop
- **GIVEN** a reconnect attempt answered with `204 No Content`
- **WHEN** the client processes it
- **THEN** it SHALL stop reconnecting and end the session normally

### Requirement: Message timeline
Every session SHALL record a timeline entry per frame or event: direction (`sent`, `received`, `system` for connect/close/reconnect), timestamp relative to session start, size in bytes, type (`text`, `binary`, `json`, `ping`, `pong`, `close`, or the SSE event type) and payload. The TUI SHALL render it with filters by direction, type and text, and pretty-print JSON payloads.

#### Scenario: Filter received JSON
- **GIVEN** a timeline of 20 sent and 80 received messages
- **WHEN** the user filters to `received` and the text `trade`
- **THEN** only received entries whose payload contains `trade` SHALL be shown

### Requirement: Timeline memory budget
The timeline SHALL count the bytes of payloads, types and ids against a memory budget of 256 MiB, dropping the oldest entries past it and always keeping the newest.

#### Scenario: Long event types and ids
- **GIVEN** SSE events whose types and ids are much larger than their data
- **WHEN** they fill the budget
- **THEN** the oldest entries SHALL be dropped and counted in `dropped_entries`

### Requirement: Headless bounds
Under `rustman run`, a session SHALL end when its steps finish and then after `limits.max_messages` (default 50) received messages or `limits.duration_ms` (default 30000), whichever comes first, closing with code 1000. Interactive sessions SHALL stay open until the user closes them.

#### Scenario: Endless feed bounded
- **GIVEN** a ws session with no steps and no limits, against a server that streams forever
- **WHEN** it runs in `rustman run`
- **THEN** it SHALL close after 50 received messages and the step SHALL complete

### Requirement: Message size limits
Incoming WebSocket messages and SSE events larger than `limits.max_message_bytes` (default 16 MiB) SHALL close the session with code 1009 (WebSocket) or abort the stream (SSE), recording `message too large: <n> bytes`. The timeline SHALL keep at most `limits.max_timeline` entries (default 10000), dropping the oldest and recording how many were dropped.

#### Scenario: Oversized frame
- **GIVEN** `max_message_bytes: 1048576` and a server sending a 2 MiB frame
- **WHEN** the frame arrives
- **THEN** the session SHALL close with code 1009 and the timeline SHALL record `message too large: 2097152 bytes`

### Requirement: Assertions and captures
The `assertions` capability SHALL apply to realtime exchanges with `json` and `body` assertions evaluated over received messages at `$.messages[*]` (each entry's parsed JSON payload, or string when not JSON), plus `messages: {count_gte | count_lte | count_eq}`, `close_code` and `latency` (time to first message). Captures SHALL support `from: json` over `$.messages`.

#### Scenario: Count and content
- **GIVEN** `- messages: {count_gte: 3}` and `- json: {path: "$.messages[0].type", equals: "snapshot"}`
- **WHEN** 5 messages arrive and the first has `type: snapshot`
- **THEN** both assertions SHALL pass

#### Scenario: Unexpected close code
- **GIVEN** `- close_code: 1000` and a server that closes with 1011
- **WHEN** assertions run
- **THEN** the failure SHALL read `close_code: expected 1000, got 1011` and the run SHALL exit with code 1

### Requirement: History recording
Each session SHALL be recorded in `request-history` as one exchange with protocol `ws` or `sse`, including handshake details, the timeline (subject to `max_timeline`), close or end reason, and redaction per the `environments` contract. Recorded payloads SHALL be cut to 64 KiB and types and ids to 1 KiB after redaction, marking the entry `truncated: true`.

#### Scenario: Token redacted
- **GIVEN** a session whose URL query carries `?token={{api_token}}` (a secret)
- **WHEN** it is recorded
- **THEN** history SHALL store the URL with `«redacted:api_token»`

### Requirement: CLI commands
`rustman ws connect <url|request-file>` SHALL run a session, read lines from stdin as text frames when stdin is not a TTY, and print received messages. `rustman sse listen <url|request-file>` SHALL print events. Both SHALL accept `--json` (one JSON object per received message or event), `--max-messages`, `--duration` and `--header`, and use `cli-runner` exit codes.

#### Scenario: Pipe to jq
- **WHEN** the user runs `rustman sse listen https://api.example.com/events --json --max-messages 3`
- **THEN** stdout SHALL contain exactly 3 JSON lines with `type`, `id`, `data` and `t_ms` fields and the exit code SHALL be 0

#### Scenario: Stdin frames
- **GIVEN** stdin piped with two lines `a` and `b`
- **WHEN** the user runs `rustman ws connect wss://echo.example.com --json`
- **THEN** two text frames SHALL be sent and their echoes printed as JSON lines

### Requirement: CLI output and stdin flow
Without `--json`, each received message SHALL be printed as received followed by a newline, so a payload holding newlines spans several lines; with `--json`, each message SHALL be one line. Stdin SHALL be read through a bounded queue of 64 frames, so that a fast input waits for the session instead of being buffered without limit.

#### Scenario: Multi-line event
- **GIVEN** an SSE event with data `line1\nline2`
- **WHEN** `rustman sse listen` prints it without `--json`
- **THEN** stdout SHALL show `line1` and `line2` on two lines, and with `--json` one line whose `data` is `"line1\nline2"`

#### Scenario: Large piped input
- **GIVEN** stdin piped from a file of 100000 lines
- **WHEN** `rustman ws connect` runs
- **THEN** at most 64 lines SHALL wait to be sent at any time

### Requirement: Agent tools
When the build includes `agent-mcp-interface`, a `realtime_listen` tool SHALL be registered that opens a ws or sse session, optionally sends initial frames, and returns the collected messages after at most 100 messages or 60 s. It SHALL pass the safety policy's host allowlist, and sending frames SHALL count as a mutation under `[agent].allow_mutations`.

#### Scenario: Read-only agent sending frames
- **GIVEN** `[agent].allow_mutations = false`
- **WHEN** an agent calls `realtime_listen` with initial frames to send
- **THEN** the call SHALL be refused with `blocked: sending frames requires allow_mutations`
