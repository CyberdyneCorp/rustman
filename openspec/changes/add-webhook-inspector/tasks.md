# Tasks

## 1. Listener core

- [x] 1.1 Create `crates/rustman-webhook` with `hyper` 1.x + `hyper-util` auto builder; bind/port-0/bind-failure handling; tests for default bind message and `cannot bind ... address in use`.
- [x] 1.2 Non-loopback detection with `--allow-remote` requirement and warning; CLI test for the refusal message.
- [x] 1.3 Configure limits (100 headers, 64 KiB header bytes → 431; 10 MiB body → 413 for declared and streamed; 10 s header / 30 s body / 60 s idle timeouts); raw-socket tests for each limit, plus TE+CL and garbage-request-line rejections with 400 (Ghostman S1/S2/S3/S4 regressions).

## 2. Captures and responses

- [x] 2.1 Capture model (id, RFC 3339 ts, remote addr, version, method, path, query, ordered duplicate headers with preserved case, raw body bytes); tests for duplicate headers, query split and non-UTF-8 body round-trip (Ghostman S7/S8 regressions).
- [x] 2.2 Ring buffer bounded by `max_captures` (500) and `max_buffer_bytes` (256 MiB) with eviction and `clear`, plus broadcast event channel; test that the 501st capture evicts the first and that exceeding the byte budget evicts oldest until under it.
- [x] 2.3 Default 200 JSON ack with `Content-Length` (Ghostman S6 regression); test exact body and header.
- [x] 2.4 Mocks from `webhook/mocks.yaml` (method/glob, status/headers/body/body_file/delay_ms ≤ 60000), first match wins, hot reload keeping previous mocks on parse error; tests for mocked 500 with delay and reload.
- [x] 2.5 Event log for captures, rejections and mock matches; test the 431 log line.

## 3. Signature verification

- [x] 3.1 GitHub `X-Hub-Signature-256` verifier with constant-time compare; tests for valid, tampered and missing.
- [x] 3.2 Stripe verifier (multiple `v1`, 300 s tolerance); tests for valid and expired using fixed clocks.
- [x] 3.3 Generic HMAC verifier (sha1/sha256/sha512, hex/base64, prefix) with secrets from `environments`; test base64 case and that secrets never appear in capture/log output.

## 4. Save and replay

- [x] 4.1 `sanitize_for_replay` dropping hop-by-hop, `Connection`-listed and framing headers; unit tests including the Ghostman B1 regression (edited body replays successfully with correct `Content-Length`).
- [x] 4.2 Replay through `rustman-http` and save as `kind: http` request file or `request-history` entry; integration test replaying to a local server and asserting `Host` is the target host.

## 5. CLI

- [x] 5.1 `rustman webhook listen` human output, `--jsonl` (one flushed JSON object per event, `body_base64` for binary), `--max-captures`, `--host/--port/--max-body`; `assert_cmd` tests piping one request; document in `docs/webhooks.md`.

## 6. TUI pane

- [x] 6.1 Webhook pane (start/stop, address/status, live list with signature badges, detail with text/JSON/hex views, event log, mock editor, save/replay/clear/copy-as-curl, non-loopback indicator) on `add-terminal-ui`; `TestBackend` snapshot tests and a test that a capture appears within 200 ms.
