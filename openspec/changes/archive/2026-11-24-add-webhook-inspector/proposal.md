# Proposal

## Why

Testing webhook producers (Stripe, GitHub, internal services) normally requires ngrok or webhook.site. A local listener that captures, verifies, mocks and replays callbacks — inside the same TUI and CLI — closes the loop for API work without third-party accounts. Ghostman proved the value but also the risk: its hand-written HTTP/1.1 parser accumulated framing bugs, unbounded memory and a replay bug (stale `Content-Length`/`Host` headers). Rustman builds this on `hyper` with limits and header hygiene from day one.

## What Changes

- A webhook listener built on `hyper` (HTTP/1.1 and HTTP/2 cleartext), started by `rustman webhook listen` or from the TUI; default bind `127.0.0.1:9090`, configurable host/port, warning when binding a non-loopback address.
- Captures any method and path; binary-safe bodies; limits: body 10 MiB → 413, header count/size → 431, per-request timeouts; in-memory ring buffer of 500 captures.
- Configurable mock responses per method/path (status, headers, body, delay), default `200 {"status":"ok"}`.
- Signature verification helpers: GitHub `X-Hub-Signature-256`, Stripe `Stripe-Signature` (with timestamp tolerance), generic HMAC — keys read from `environments` secrets.
- Save a capture to `request-history` or as a request file; replay a capture to any URL with hop-by-hop/framing header hygiene (Ghostman B1 regression).
- CLI JSON-lines output for piping (`rustman webhook listen --jsonl | jq`); TUI webhook pane (layout from `add-terminal-ui`).

## Capabilities

### New Capabilities

- `webhook-inspector`: a local listener that captures, verifies, mocks and replays inbound HTTP callbacks, with resource limits, CLI streaming output and a TUI pane.

### Modified Capabilities

None (uses `environments` secrets, `http-requests` for replay, `workspace` request files and `request-history` without changing their requirements).

## Out of Scope

- Public tunneling / exposing the listener to the internet (users can pair it with their own tunnel).
- HTTPS termination on the listener (a later option; most providers deliver to tunnels that terminate TLS).
- Persisting the ring buffer across restarts (only explicit saves persist).
- Rate limiting and multi-tenant hosting.

## Impact

- New crate `crates/rustman-webhook`; `webhook` subcommand in `crates/rustman-cli`; TUI pane in `crates/rustman-tui`.
- Dependencies: `hyper` 1.x + `hyper-util` (already used by `rustman-http`), `hmac` + `sha2` (RustCrypto), `subtle` for constant-time comparison.
- Workspace config gains an optional `[webhook]` section in `rustman.toml` and `webhook/mocks.yaml`.
