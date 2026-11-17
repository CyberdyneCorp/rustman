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

### D2. Header order and duplicates
Use `hyper`'s `HeaderMap` iteration, which preserves insertion order per name and duplicates; store as `Vec<(HeaderName, HeaderValue)>` with raw bytes values (`HeaderValue::as_bytes`) to remain binary-safe. Original header-name casing is not preserved by HTTP/2 and only optionally by hyper HTTP/1 (`preserve_header_case`) — enable it for HTTP/1 so captures show the sender's casing.

### D3. Capture store
`Arc<Mutex<VecDeque<Capture>>>` with capacity `max_captures` plus a `tokio::sync::broadcast` channel of `Event`s for subscribers (TUI, JSONL writer). Bodies are `bytes::Bytes` (cheap clone for replay/save). Worst-case memory = `max_captures × max_body`, documented; defaults (500 × 10 MiB) are an upper bound users can lower.

### D4. Signature verification
RustCrypto `hmac` + `sha2`/`sha1`, comparison with `subtle::ConstantTimeEq`. Verifiers attach per path pattern in `rustman.toml` `[[webhook.verifiers]]`. Verification runs on the raw body bytes before any decoding. Secrets resolved lazily through the `environments` secret store; values never enter the capture or logs.

### D5. Replay hygiene in one function
A single `sanitize_for_replay(headers) -> headers` drops the hop-by-hop set (RFC 9110 §7.6.1 list plus names listed in `Connection`) and framing headers (`Host`, `Content-Length`, `Transfer-Encoding`). Both "replay" and "save as request file" call it, so B1 can't regress through one path only. Regression test lives next to it.

### D6. Mocks
`webhook/mocks.yaml` parsed into an ordered list of `(method, glob, response)`; `globset` for patterns; hot reload with `notify` (debounced 200 ms); a parse error keeps the previous mocks active and logs the error.

## Risks / Trade-offs

- [Large captures exhausting memory] → `max_buffer_bytes` (default 256 MiB) bounds total body bytes alongside `max_captures`; the TUI shows current buffer bytes.
- [hyper rejects some sloppy real-world senders] → that's desirable for a strict inspector; rejection reasons are surfaced in the event log.
- [Non-loopback exposure] → explicit `--allow-remote`, TUI confirmation, persistent indicator.

## Migration Plan

Additive; new `[webhook]` config section and optional `webhook/mocks.yaml`.

## Open Questions

- Should HTTPS on the listener (self-signed, generated per workspace) be added for providers that refuse plain HTTP to localhost tunnels? Deferrable to a follow-up change.
