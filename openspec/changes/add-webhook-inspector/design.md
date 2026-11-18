# Design

## Context

Ghostman's webhook receiver hand-parsed HTTP/1.1 over raw sockets; its baseline spec and backlog list the consequences: invalid JSON error bodies, accepted TE+CL requests, garbage request lines captured, silent drops on long header lines, unbounded capture memory, collapsed duplicate headers, lossy UTF-8 decoding, and replay failures from stale `Content-Length`/`Host` (B1). This design exists mostly to make those structurally impossible. Replay uses the `rustman-http` engine from `bootstrap-core-engine`; secrets come from `environments`.

## Goals / Non-Goals

**Goals:**
- No hand-written HTTP parsing; every limit enforced before memory is committed.
- Captures are lossless (bytes, header order, duplicates).
- The same capture store feeds the TUI, CLI JSONL and saves.

**Non-Goals:**
- TLS on the listener, tunneling, persistence of the ring buffer.

## Decisions

### D1. `hyper` 1.x server via `hyper-util` auto builder (rejected: `axum`, hand-rolled, `tiny_http`)
`hyper-util::server::conn::auto` serves HTTP/1.1 and h2c from one listener and rejects malformed framing (including TE+CL) per RFC 9112. `axum` adds routing we don't need (every path is captured) and hides header order. `tiny_http` is synchronous and less strict. Limits: `http1::Builder::max_headers(100)` and `max_buf_size` for header bytes → hyper returns 431; body limit via `http_body_util::Limited` → 413; `header_read_timeout(10s)` on the HTTP/1 builder; body timeout via `tokio::time::timeout` around collection.

HTTP/1 keep-alive is off: each HTTP/1 connection carries one request, so the connection's first bytes are that request's head (see D2) and an error response can close the connection without ambiguity. hyper silently prefers `Transfer-Encoding` over `Content-Length` and drops the latter, so TE+CL is detected on the raw head (D2) and rejected with 400. A refused connection keeps reading and discarding input for up to 2 s after the response, so the client reads the 413/408 instead of a reset. The 60 s idle limit is a per-connection watchdog (no traffic and no request in flight), which matters for h2c connections.

### D2. Header order and duplicates
Use `hyper`'s `HeaderMap` iteration, which preserves insertion order per name and duplicates; store as `Vec<(HeaderName, HeaderValue)>` with raw bytes values (`HeaderValue::as_bytes`) to remain binary-safe. Original header-name casing is not preserved by HTTP/2 and only optionally by hyper HTTP/1 (`preserve_header_case`) — enable it for HTTP/1 so captures show the sender's casing. Implementation note: hyper 1.x keeps its case map private and groups different names together, so the listener keeps a copy of each HTTP/1 connection's first bytes (bounded by the header-byte limit) and reads the head again with `httparse`, the parser hyper itself uses, after hyper has validated it. This yields the sender's case and the true arrival order. HTTP/2 headers come from hyper's `HeaderMap`.

### D3. Capture store
`Arc<Mutex<VecDeque<Arc<Capture>>>>` bounded by both `max_captures` (default 500) and `max_buffer_bytes` (default 256 MiB of body bytes): each push evicts the oldest captures until both bounds hold, always keeping the newest, so the buffer holds at most `max_buffer_bytes + max_body`. A `tokio::sync::broadcast` channel of `Event`s (backlog 1024) feeds subscribers (TUI, CLI); a subscriber that lags keeps evicted `Arc<Capture>`s alive until it catches up or the backlog overwrites them, so the true worst case adds `1024 × max_body` while a subscriber lags. Both front-ends drain the channel without blocking: the TUI reduces each event to its log line and a changed-list flag before forwarding it to its (unbounded) action loop, so a stalled render loop holds no captures, and the CLI hands them to a printing thread through a queue bounded by 256 events and 32 MiB of bodies, dropping (and counting) what does not fit. The CLI never reads the buffer back, so it runs with `BufferLimits::events_only()` (the newest capture only). Bodies are `bytes::Bytes` (cheap clone for replay/save).

### D4. Signature verification
RustCrypto `hmac` + `sha2`/`sha1`, comparison with `subtle::ConstantTimeEq`. Verifiers attach per path pattern in `rustman.toml` `[[webhook.verifiers]]`. Verification runs on the raw body bytes before any decoding. Secrets resolved lazily through the `environments` secret store; values never enter the capture or logs. Implementation note: secrets are resolved once when the listener starts (a missing secret stops it before binding), `secret` must be a `{{variable}}` reference resolved from the active environment only (never `[variables]` in `rustman.toml`) so keys never sit in `rustman.toml`, and front-ends redact output with a `Redactor` that knows every verifier key.

### D5. Replay hygiene in one function
A single `sanitize_for_replay(headers) -> headers` drops the hop-by-hop set (RFC 9110 §7.6.1 list plus names listed in `Connection`) and framing headers (`Host`, `Content-Length`, `Transfer-Encoding`). Both "replay" and "save as request file" call it, so B1 can't regress through one path only. Regression test lives next to it. Replay builds a `ResolvedRequest` for `HttpEngine`, which computes `Host` and `Content-Length`; a body that is not UTF-8 is staged in a private temporary file and sent as a file body, byte for byte. A saved request file writes such a body to a sidecar file, escapes `{{` in captured text, and leaves out always-sensitive headers (`Authorization`, `Cookie`, ...) because request files are meant for Git.

### D6. Mocks
`webhook/mocks.yaml` parsed into an ordered list of `(method, glob, response)`; `globset` for patterns; hot reload with `notify` (debounced 200 ms); a parse error keeps the previous mocks active and logs the error.

### D7. Front-ends
`rustman webhook listen` and the TUI pane are thin adapters over `rustman-webhook`: the JSON-lines shape (`jsonl::event_json`), copy-as-curl (`curl::copy_as_curl`), the listener settings of `[webhook]` (`WebhookSettings::listener_config`) and the verifier loading (`Verifiers::load`: environment, secret store, environment-only secrets) live in the library. `--max-captures` stops accepting after the Nth capture and lets replies in flight finish (`ListenerHandle::finish`, hyper graceful shutdown) before exiting. The TUI runs the listener on its own thread with its own runtime and forwards store events to the UI's action channel, so a capture reaches the pane on the next frame; a network-reachable address is confirmed in a dialog before binding (rejected: an `--allow-remote`-style toggle in `tui.toml`, which would make exposure silent).

## Risks / Trade-offs

- [Large captures exhausting memory] → `max_buffer_bytes` (default 256 MiB) bounds total body bytes alongside `max_captures`; the TUI shows current buffer bytes.
- [Bodies in flight] → limits are per request: bodies being received are bounded by `max_body` times the requests in flight (connections, and HTTP/2 streams per connection), with no overall budget; documented in docs/webhooks.md.
- [HTTP/2 header lists refused by h2 itself] → h2's own list limit is set to twice the header-byte limit plus HPACK's 32 bytes per field, so lists over the configured limits reach the service and are logged; only larger ones are answered 431 by h2 without a log line.
- [hyper rejects some sloppy real-world senders] → that's desirable for a strict inspector; rejection reasons are surfaced in the event log.
- [Non-loopback exposure] → explicit `--allow-remote`, TUI confirmation, persistent indicator.

## Migration Plan

Additive; new `[webhook]` config section and optional `webhook/mocks.yaml`.

## Open Questions

- Should HTTPS on the listener (self-signed, generated per workspace) be added for providers that refuse plain HTTP to localhost tunnels? Deferrable to a follow-up change.
