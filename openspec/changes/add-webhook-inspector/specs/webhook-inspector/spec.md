# Spec Delta

## Purpose

Provides a local HTTP listener that captures, verifies, mocks and replays inbound webhooks so users can test webhook producers without third-party services, from the TUI or the CLI.

## ADDED Requirements

### Requirement: Listener lifecycle and bind address
The listener SHALL bind `127.0.0.1:9090` by default; host and port SHALL be configurable via `--host`/`--port` or `[webhook]` in `rustman.toml`. Port `0` SHALL pick a free port and print it. A bind failure SHALL report `webhook: cannot bind <addr>: <reason>` and leave the listener stopped.

#### Scenario: Default start
- **WHEN** the user runs `rustman webhook listen`
- **THEN** the listener SHALL accept connections on `127.0.0.1:9090` only and print `Listening on http://127.0.0.1:9090`

#### Scenario: Port in use
- **GIVEN** port 9090 is taken
- **WHEN** the user starts the listener
- **THEN** the error `webhook: cannot bind 127.0.0.1:9090: address in use` SHALL be shown and the exit code SHALL be non-zero

### Requirement: Non-loopback bind warning
Binding any non-loopback address (e.g. `0.0.0.0`, a LAN IP) SHALL print a warning that the listener is reachable from the network; the TUI SHALL require confirmation before starting, and the CLI SHALL require `--allow-remote`.

#### Scenario: CLI without opt-in
- **WHEN** the user runs `rustman webhook listen --host 0.0.0.0`
- **THEN** it SHALL refuse to start with `webhook: binding 0.0.0.0 exposes the listener to the network; pass --allow-remote`

### Requirement: Standards-compliant HTTP handling
The listener SHALL use a maintained HTTP implementation for all parsing and framing (HTTP/1.1 including chunked encoding, and HTTP/2 cleartext with prior knowledge). Malformed requests (invalid request line, conflicting `Content-Length`, `Transfer-Encoding` with `Content-Length`) SHALL be rejected with 400 and SHALL NOT be captured.

#### Scenario: Conflicting framing rejected
- **WHEN** a client sends both `Transfer-Encoding: chunked` and `Content-Length: 10`
- **THEN** the response SHALL be 400 and no capture SHALL be recorded

#### Scenario: Chunked body captured
- **WHEN** a client sends a valid chunked body `abc`
- **THEN** the capture SHALL contain body bytes `abc`

### Requirement: Capture any method and path
Every well-formed request, regardless of method or path, SHALL be captured with: sequence id, receive timestamp (RFC 3339, local offset), remote address, HTTP version, method, path, query string, ordered list of headers (duplicates preserved), body bytes, and body size.

#### Scenario: Duplicate headers preserved
- **WHEN** a request carries `X-A: 1` and `X-A: 2`
- **THEN** the capture SHALL list both header entries in arrival order

#### Scenario: Query string captured
- **WHEN** a client POSTs to `/hooks/stripe?debug=1`
- **THEN** the capture SHALL have path `/hooks/stripe` and query `debug=1`

### Requirement: Binary-safe bodies
Bodies SHALL be stored as raw bytes. Display SHALL use UTF-8 text (pretty-printed when JSON, form fields when `application/x-www-form-urlencoded`) when the body decodes cleanly, and a hex dump otherwise; no bytes SHALL be dropped or replaced in storage, saves or replays.

#### Scenario: Non-UTF-8 body
- **WHEN** a client posts bytes `0xFF 0xFE 0x00`
- **THEN** the capture SHALL store exactly those 3 bytes and display them as hex

### Requirement: Body size limit
Bodies larger than `max_body` (default 10 MiB, configurable) SHALL be rejected with `413 Payload Too Large` as soon as the limit is exceeded (declared length or streamed bytes) and SHALL NOT be captured; the rejection SHALL appear in the event log.

#### Scenario: Declared oversize
- **WHEN** a request declares `Content-Length: 20971520`
- **THEN** the response SHALL be 413 without reading the body

#### Scenario: Streamed oversize
- **WHEN** a chunked request streams more than 10 MiB
- **THEN** the response SHALL be 413 and the connection SHALL be closed

### Requirement: Header limits
Requests with more than 100 header fields or more than 64 KiB of total header bytes (defaults, configurable) SHALL be rejected with `431 Request Header Fields Too Large` and SHALL NOT be captured.

#### Scenario: Too many headers
- **WHEN** a request carries 150 header fields
- **THEN** the response SHALL be 431

### Requirement: Timeouts
The listener SHALL close connections that do not complete request headers within 10 s or the body within 30 s (defaults, configurable), responding 408 when a response can still be written. Idle keep-alive connections SHALL close after 60 s.

#### Scenario: Slow headers
- **GIVEN** a client that sends a partial header block and stalls
- **WHEN** 10 s elapse
- **THEN** the connection SHALL be closed and no capture recorded

### Requirement: Bounded capture buffer
Captures SHALL be kept in an in-memory ring buffer bounded by `max_captures` (default 500) and `max_buffer_bytes` (default 256 MiB of body bytes); when either bound would be exceeded, the oldest captures SHALL be evicted until both hold. A `clear` action SHALL empty the buffer. Saved captures SHALL be unaffected by eviction.

#### Scenario: Eviction
- **GIVEN** a buffer holding 500 captures
- **WHEN** a new request arrives
- **THEN** the oldest capture SHALL be evicted and the buffer SHALL still hold 500

#### Scenario: Byte budget eviction
- **GIVEN** `max_buffer_bytes` 256 MiB and 30 buffered captures of 9 MiB each (270 MiB would exceed it)
- **WHEN** the 30th capture arrives
- **THEN** the oldest captures SHALL be evicted until total body bytes are at most 256 MiB

### Requirement: Default response
Without a matching mock, the listener SHALL respond `200 OK` with `Content-Type: application/json`, a correct `Content-Length`, and body `{"status":"ok","source":"rustman"}`.

#### Scenario: Default ack
- **WHEN** a client posts to `/anything`
- **THEN** the response SHALL be 200 with body `{"status":"ok","source":"rustman"}` and a matching `Content-Length`

### Requirement: Mock responses
Mocks defined in `webhook/mocks.yaml` or the TUI SHALL match by method (or `*`) and path pattern (exact or glob with `*`/`**`), first match wins, and respond with configured `status`, `headers`, `body` (inline or `body_file`) and `delay_ms` (max 60000). Mock files SHALL reload on change without restarting the listener.

#### Scenario: Mocked failure
- **GIVEN** mock `POST /hooks/github` → `status: 500`, `delay_ms: 2000`
- **WHEN** GitHub posts to `/hooks/github`
- **THEN** the response SHALL be 500 after 2 s and the request SHALL still be captured

#### Scenario: Hot reload
- **GIVEN** a running listener
- **WHEN** the user edits `webhook/mocks.yaml`
- **THEN** the next matching request SHALL use the new mock

### Requirement: Event log
Every capture, rejection (400/408/413/431, with reason) and mock match SHALL be recorded in an event log shown in the TUI and emitted by the CLI.

#### Scenario: Rejection logged
- **WHEN** a request is rejected with 431
- **THEN** the event log SHALL show the remote address, `431` and `too many header fields`

### Requirement: GitHub signature verification
When a verifier `github` is configured for a path with secret `{{secret_name}}`, the capture SHALL be marked `signature: valid` if `X-Hub-Signature-256` equals `sha256=` + hex HMAC-SHA256 of the raw body, compared in constant time; otherwise `invalid` or `missing`.

#### Scenario: Valid GitHub signature
- **GIVEN** verifier `github` with secret `gh_hook`
- **WHEN** a request arrives with a correct `X-Hub-Signature-256`
- **THEN** the capture SHALL be marked `signature: valid`

#### Scenario: Tampered body
- **WHEN** the body differs from what was signed
- **THEN** the capture SHALL be marked `signature: invalid`

### Requirement: Stripe signature verification
A `stripe` verifier SHALL parse `Stripe-Signature` (`t=`, one or more `v1=`), compute HMAC-SHA256 over `<t>.<raw body>`, accept if any `v1` matches in constant time, and mark `expired` when `t` is older than the tolerance (default 300 s).

#### Scenario: Expired timestamp
- **GIVEN** a correctly signed Stripe event with `t` 10 minutes old
- **WHEN** it is verified
- **THEN** the capture SHALL be marked `signature: expired`

### Requirement: Generic HMAC verification
An `hmac` verifier SHALL take `header`, `algorithm` (`sha256` default, `sha1`, `sha512`), `encoding` (`hex` default, `base64`), optional `prefix`, and a secret, and mark captures `valid`, `invalid` or `missing`. Secrets SHALL be resolved from `environments` and never displayed.

#### Scenario: Base64 HMAC
- **GIVEN** verifier `hmac` with `header: X-Signature`, `encoding: base64`
- **WHEN** a request carries a correct base64 HMAC
- **THEN** the capture SHALL be marked `signature: valid`

### Requirement: Save captures
The user SHALL be able to save a capture to `request-history` (as an inbound exchange) or as an `http` request file targeting a chosen URL; saved request files SHALL exclude hop-by-hop and framing headers per the replay hygiene rule.

#### Scenario: Save as request file
- **GIVEN** a capture of `POST /hooks/stripe` with JSON body
- **WHEN** the user saves it as a request file targeting `{{api}}/hooks/stripe`
- **THEN** a `kind: http` request file SHALL be written with method, body and headers minus excluded headers

### Requirement: Replay with header hygiene
Replaying a capture to a URL SHALL send the original method, body bytes and headers through `http-requests`, removing `Host`, `Content-Length`, `Transfer-Encoding`, `Connection`, `Keep-Alive`, `Proxy-Connection`, `TE`, `Trailer`, `Upgrade` and headers named in `Connection`. Replay SHALL succeed after the body is edited.

#### Scenario: Replay after editing body
- **GIVEN** a capture with `Content-Length: 7` and body `{"a":1}`
- **WHEN** the user edits the body to `{"a":12345}` and replays it
- **THEN** the request SHALL be sent with a correct `Content-Length` and SHALL NOT fail on framing

#### Scenario: Host rewritten
- **WHEN** a capture received with `Host: 127.0.0.1:9090` is replayed to `https://staging.example.com/hook`
- **THEN** the sent `Host` SHALL be `staging.example.com`

### Requirement: CLI streaming output
`rustman webhook listen` SHALL print one human-readable line per event by default and, with `--jsonl`, exactly one JSON object per line per event (`type`, `id`, `ts`, `method`, `path`, `status`, `signature`, `headers`, `body` as UTF-8 or `body_base64`), flushing after each line. `--max-captures N` SHALL exit 0 after N captures.

#### Scenario: Pipe to jq
- **WHEN** the user runs `rustman webhook listen --jsonl | jq .path` and one request arrives at `/x`
- **THEN** `"/x"` SHALL be printed immediately

#### Scenario: Stop after N
- **WHEN** the user runs `rustman webhook listen --max-captures 1` and one request arrives
- **THEN** the process SHALL exit with code 0

### Requirement: TUI webhook pane
The TUI SHALL provide a webhook pane with start/stop control, bind address and status, live capture list (time, method, path, size, signature badge), capture detail (headers, body view text/JSON/hex), event log, mock editor, and actions save, replay, clear and copy-as-curl. A persistent indicator SHALL show when the listener runs on a non-loopback address.

#### Scenario: Live capture
- **GIVEN** the listener is running in the TUI
- **WHEN** a request arrives
- **THEN** it SHALL appear at the top of the capture list within 200 ms with its signature badge
