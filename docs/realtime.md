# WebSocket and Server-Sent Events

Rustman opens WebSocket sessions and listens to Server-Sent Events (SSE) streams from saved request files: chat and trading feeds, notifications, dashboards and LLM token streams. Each session records a message timeline, ends on its own under `rustman run`, and takes assertions and captures over the messages it received. The file format is in [request-files.md](request-files.md#websocket-and-sse-requests); the behavior is specified in [`openspec/specs/realtime-sessions`](https://github.com/CyberdyneCorp/rustman/blob/main/openspec/specs/realtime-sessions/spec.md). GraphQL subscriptions are covered by [GraphQL](graphql.md) and JSON-RPC subscriptions by [JSON-RPC](jsonrpc.md).

## WebSocket sessions

A `kind: ws` session opens with the RFC 6455 handshake through the same stack as HTTP requests: the file's headers, auth helpers and the cookie jar go into the upgrade request, `[http]` proxies are tunnelled (HTTP `CONNECT`, HTTPS, SOCKS5), and `wss://` uses TLS with verification on and the file's `tls` settings (`ca_bundle`, client certificates, `verify: false` for `insecure`). A certificate that does not verify fails with a TLS error naming the host. `subprotocols` are offered in order in `Sec-WebSocket-Protocol`; the handshake response status, headers and the subprotocol the server chose are recorded. No WebSocket extension is offered: `permessage-deflate` is not negotiated.

A server that answers the upgrade with another status than `101` fails the session with `ws: handshake failed: <status> <reason>`, such as `ws: handshake failed: 401 Unauthorized` (exit code 3 under `rustman run`); its headers and body (up to 64 KiB) are recorded.

Once open, the session runs its `steps` in order:

```yaml
version: 1
kind: ws
name: Orders
url: wss://{{host}}/orders
steps:
  - send: {json: {id: 1, op: subscribe, channel: orders}}
  - wait_for: {json: {path: $.id, equals: 1}, timeout_ms: 2000}
  - send: {ping: aGk=}
  - sleep: 500
  - close: {code: 1000, reason: done}
```

- `send` sends a text frame, a JSON value (serialized compactly as a text frame), a binary frame from base64 or from `{file: <path>}` (a file larger than `limits.max_message_bytes` is refused before it is read), or a ping (at most 125 bytes of data). Sending on a closed socket fails with `ws: not connected`.
- `wait_for` waits for a message that arrives after the step starts and matches its JSONPath check (`equals`, `matches` or `exists`, with the same evaluator as `json` assertions) or its regular expression. A message that is not JSON never matches a JSONPath check. After `timeout_ms` (default 5000) the session fails with `ws: wait_for timed out after <n> ms (<matcher>)`, such as `ws: wait_for timed out after 300 ms (regex pong)`.
- `sleep` waits, recording what arrives.
- `close` sends a close frame (code default 1000; a reason of at most 123 bytes, as a control frame allows), waits up to 2000 ms for the server's close, then drops the connection.

Messages that arrive while no `wait_for` is active are still recorded. Server pings are answered automatically; the pong to one of our pings is recorded with its round-trip time (`rtt_ms`). A close from the server ends the session and is recorded with its code and reason, such as `closed by server: 1008 policy`.

## SSE sessions

A `kind: sse` session sends its request (GET by default, or any method with a body, such as the POST of an LLM API) through the HTTP engine, with the same auth helpers, signing, cookies, TLS and proxies as `kind: http`, `Accept: text/event-stream` unless the file sets `Accept`, and `Accept-Encoding: identity` so the stream arrives as it is written. The response must be a success with `Content-Type: text/event-stream`; otherwise the session fails with `sse: unexpected content type <type>` (for example `sse: unexpected content type application/json`) or `sse: unexpected status <status> <reason>`.

The stream is parsed per the WHATWG HTML specification, by the same parser GraphQL over SSE uses:

- lines end with CRLF, LF or CR, and a leading byte order mark is skipped;
- lines starting with `:` are comments;
- `event` sets the event type (default `message`), `data` lines are joined with `\n`, `id` sets the event's id (kept for the following events; an `id` holding NUL is ignored), and `retry` (digits only) sets the reconnection delay;
- an `event` or `id` value longer than 4 KiB is ignored, as if the line were absent: both are copied into every event they apply to, so they stay small;
- a blank line dispatches the event, when it has data, and only then does its `id` become the last event id: an event the connection cut before its blank line never moves `Last-Event-ID`.

The stream `event: update\ndata: line1\ndata: line2\nid: 7\n\n` records one event of type `update`, id `7` and data `line1\nline2`.

### Reconnect and resume

When the stream ends or the connection drops, the session reconnects after the last `retry` value (default 3000 ms; a larger value than 60000 ms is capped at 60000 ms, with an `error` note in the timeline), sending `Last-Event-ID` with the last event id received, up to `reconnect.max_attempts` times (default 5; `0` disables reconnecting). `last_event_id` in the file sets the header of the first connection. A reconnection answered with `204 No Content` stops reconnecting and ends the session normally. Each reconnection is a `system` entry of the timeline, such as `reconnected (attempt 1, Last-Event-ID: 42)`.

## The timeline

Every frame, event and connection change is a timeline entry:

| Field | Meaning |
|---|---|
| `dir` | `sent`, `received`, or `system` (connect, reconnect, close, errors). |
| `t_ms` | Milliseconds since the session started. |
| `size` | The payload size in bytes. |
| `type` | `text`, `json`, `binary`, `ping`, `pong`, `close`, a system note (`connect`, `reconnect`, `error`) or the SSE event type. |
| `payload` | The text; binary data in base64. |
| `id` | The SSE event id. |
| `rtt_ms` | The round-trip time of a pong answering one of our pings. |

The timeline keeps at most `limits.max_timeline` entries (default 10000) and at most 256 MiB of payloads, types and ids, so memory stays bounded however large the messages are: the oldest entries are dropped and counted in `dropped_entries` (the newest entry is always kept). The TUI's live view keeps the same bounds.

Entries reach the terminal (`rustman ws connect`, `rustman sse listen`) and the TUI's live view through a bounded feed. At most 1024 entries, holding at most 64 MiB, wait for the display; a single larger entry is still shown when nothing else waits. A display slower than the messages never slows the session down. Entries that do not fit are not shown, and a note says how many were skipped, in place: on stderr, `warning: output fell behind: <n> timeline entries not shown (the session's timeline and history keep them)`, and in the TUI a system entry, `<n> entries not shown: the display fell behind`. Skipped entries are still recorded in the timeline, so assertions, history and reports see every one of them.

## Bounds and limits

Under `rustman run` a session runs its steps, then listens until `limits.max_messages` received messages (default 50) or `limits.duration_ms` since it started (default 30000), whichever comes first, and closes with code 1000. The end is recorded as `max_messages reached (50)` or `duration reached (30000 ms)`. A server that streams forever is therefore always bounded. Ctrl+C closes the session.

A WebSocket message larger than `limits.max_message_bytes` (default 16 MiB) closes the session with code 1009 (the rest of what the server sends is read and discarded until it closes the connection, for at most 2000 ms, so the close frame reaches it), and an SSE event larger than it aborts the stream; the timeline records `message too large: <n> bytes` and the session fails. This limit is the only one for realtime sessions: `[http] max_body_bytes` does not apply to them.

The bound names `limits.max_messages` and `limits.duration_ms` differ from the top-level `max_events` and `max_duration_ms` of GraphQL and JSON-RPC subscriptions.

## Assertions and captures

Realtime requests take the `assertions` and `captures` of [assertions.md](assertions.md), evaluated after the session ends, over a document whose `messages` member lists every message received, in order: its payload parsed as JSON, or its text when it is not JSON. Control frames (ping, pong, close) and system entries are not messages.

| Assertion | Meaning |
|---|---|
| `json: {path: '$.messages[0].type', equals: snapshot}` | A JSONPath check over the messages. |
| `body: {contains: trade}` | A text check over the messages document. |
| `messages: {count_gte: 3}` | How many messages were received (`count_gte`, `count_lte` or `count_eq`), dropped entries included. |
| `close_code: 1000` | The close code either side sent. A failure reads `close_code: expected 1000, got 1011`. |
| `latency: {ttfb_lt: 500}` | `ttfb_lt` bounds the time to the first message; `total_lt` the whole session. |
| `status`, `header` | The handshake response (WebSocket) or the stream's response head (SSE). |

```yaml
version: 1
kind: sse
name: Prices
url: https://{{host}}/prices
limits: {max_messages: 5}
assertions:
  - messages: {count_eq: 5}
  - json: {path: '$.messages[0].symbol', exists: true}
captures:
  - {var: first_symbol, from: json, path: '$.messages[0].symbol'}
```

A session that fails (a refused handshake, a `wait_for` timeout, an oversized message) errors the request instead, and its assertions are not evaluated.

## History and reports

Each session is one history entry and one report request, with the opening request under `sent` and a `realtime` record:

```json
{
  "protocol": "ws",
  "handshake": {"status": 101, "headers": [], "subprotocol": "v1.feed"},
  "timeline": [
    {"dir": "system", "t_ms": 3, "size": 0, "type": "connect", "payload": "connected (subprotocol v1.feed)"},
    {"dir": "sent", "t_ms": 4, "size": 26, "type": "json", "payload": "{\"op\":\"subscribe\",\"n\":1}"}
  ],
  "dropped_entries": 0,
  "received": 0,
  "end": {"reason": "closed by client: 1000", "close_code": 1000},
  "elapsed_ms": 12.5
}
```

Payloads larger than 64 KiB are kept as a preview with `truncated: true`; `size` keeps the full size. Types and ids (SSE event names and ids) are cut to 1 KiB the same way, also marking the entry `truncated: true`. Each payload (and a refused upgrade's body) is redacted whole before it is cut, so a secret that crosses the 64 KiB mark never leaves a part of itself in the preview. Everything is redacted like any other record: secrets in the URL, headers, payloads, types (SSE event names), ids and close reasons appear as `«redacted:<name>»`, and so do the credentials the opening request carried on the wire (auth helpers, cookies from the jar) or the response set, so a URL with `?token={{api_token}}` is stored as `?token=«redacted:api_token»`, and sensitive handshake headers (such as `Set-Cookie`) are hidden. Sessions cannot be replayed with `rustman history replay`. The `realtime` member is part of the [report schema](schemas/report.schema.json).

## `rustman ws connect` and `rustman sse listen`

`rustman ws connect <url|request>` opens a WebSocket session and prints each message the server sends; `rustman sse listen <url|request>` prints each event of a stream. The target is a URL (or a `{{variable}}` holding one), or a saved `kind: ws` / `kind: sse` request by file, id or name, which runs with its steps, assertions and captures:

```sh
rustman ws connect wss://echo.example.com --json --max-messages 2 < frames.txt
rustman ws connect {{feed_url}} --header "Authorization: Bearer {{token}}" --subprotocol v2.feed
rustman sse listen https://api.example.com/events --json --max-messages 3 | jq .data
rustman sse listen streams/completions.rman.yaml --env prod
```

- When stdin is not a terminal, `ws connect` sends each line of it as a text frame (without its line ending, and without interpolation), while the session runs. At most 64 lines wait to be sent: a faster stdin (a large file) is read only as the session sends.
- Messages are printed as they arrive, each followed by a newline: the payload as received (binary data in base64), so a payload that holds newlines (multi-line SSE `data`, pretty-printed JSON) spans several lines. With `--json`, each message is one JSON object on one line (newlines in the payload are escaped), ready for `jq`: `{"t_ms": 12, "dir": "received", "type": "tick", "size": 7, "id": "1", "data": "{\"n\":1}"}`. `type` is the frame type (`text`, `json`, `binary`) or the SSE event type, and `id` is the SSE event id (`null` without one). Sent frames, pings, pongs and system notes are not printed; they are in the session's history record.
- Sessions are bounded as under `rustman run`: `--max-messages <n>` and `--duration <duration>` replace the request's `limits.max_messages` and `limits.duration_ms` (defaults 50 messages and 30 s), and the session then closes with code 1000. Ctrl+C closes it at once.
- `--header "Name: value"` (repeatable) adds handshake or request headers to a URL; `--subprotocol <name>` (repeatable) offers WebSocket subprotocols in order. A saved request already defines both, so these flags are refused with one. `--env`, `--workspace`, `--var`, `--insecure`, `--no-history` and `--trace` (send [trace context](tracing.md) on the handshake or stream request) work as for `rustman send`.
- Everything printed comes from the session's redacted feed: a secret the server echoes appears as `«redacted:<name>»`. The session is recorded in history like any request (outside a workspace, nothing is recorded).

Exit codes: 0 when the session ended normally (its steps done, a bound reached, or closed by the server), 1 when an assertion of the request failed, 2 for a usage error (such as a request of another kind), 3 when the session failed (`ws: handshake failed: 401 Unauthorized`, a transport error, a `wait_for` timeout, an oversized message), and 130 on Ctrl+C.

## In the TUI

`kind: ws` and `kind: sse` requests open in the realtime editor of the [terminal UI](tui.md#the-websocket-and-sse-editor): `Ctrl+R` connects and the Timeline tab lists the entries as they arrive, with filters by direction, type and text and the selected payload pretty-printed; the Compose tab sends text, JSON or binary frames while the session is open, and `Esc` closes it. Interactive sessions have no headless bounds: they stay open until you close them or the server does.

## Agent tool

The [agent MCP server](agent.md) registers the `realtime_listen` tool. It takes either `path` (a saved `kind: ws` or `kind: sse` request) or `url` (`ws://` / `wss://` for a WebSocket, `http://` / `https://` for an SSE stream; a URL that starts with a `{{variable}}` opens an SSE stream unless `frames` are given) with optional `frames` (a string is a text frame, any other value is sent as JSON), plus `max_messages`, `duration_ms` and `vars`. It returns the received messages (`t_ms`, `type`, `id`, `data`), how the session ended and the request's assertions, all redacted.

- A session stops after at most 100 messages or 60 s, whatever `max_messages`, `duration_ms` or the file's `limits` ask for.
- The tool follows no live feed: it reads the session's timeline once the session ends, so it has the same bounds as the timeline.
- The URL must pass the agent allowlist (`[agent] allowed_hosts`, or the hosts of the active environment); a `ws://` URL is checked as its `http://` origin and `wss://` as `https://`, and the environment's `ws(s)://` values count as those origins. Otherwise the call is refused with `blocked: host not allowed` and nothing is opened.
- Sending frames is a mutation, whether they come from `frames` or from the `send` steps of a saved request: with `[agent] allow_mutations = false` (the default) the call is refused with `blocked: sending frames requires allow_mutations`, and otherwise the human confirms it as for any mutating request. An SSE `POST` stream is a mutation too.
- Sessions are recorded in history like any request.
