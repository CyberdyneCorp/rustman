# Webhook inspector

`rustman webhook listen` runs a local HTTP listener that captures every inbound request (any method, any path), answers it with a default acknowledgement or a mock you configure, and prints an event log. It is meant for testing webhook producers (GitHub, Stripe, your own services) without a third-party service; pair it with your own tunnel if the producer is on the internet.

The behavior is specified in [`openspec/specs/webhook-inspector`](https://github.com/CyberdyneCorp/rustman/blob/main/openspec/specs/webhook-inspector/spec.md).

## Starting the listener

```text
$ rustman webhook listen
Listening on http://127.0.0.1:9090
#1 POST /hooks/github from 127.0.0.1:53114 (1832 B) -> 200
```

| Flag | Effect |
|------|--------|
| `--host <HOST>` | Address to bind: an IP address or `localhost` (default: `[webhook] host` in `rustman.toml`, else `127.0.0.1`). |
| `--port <PORT>` | Port to bind (default: `[webhook] port`, else `9090`); `0` picks a free port and the banner shows it. |
| `--allow-remote` | Required to bind an address reachable from the network, such as `0.0.0.0` or a LAN IP. |
| `--max-body <BYTES>` | Largest accepted body (default 10485760, 10 MiB); larger bodies get `413`. |
| `--max-captures <N>` | Exit with code 0 once N requests were captured. Events already queued for the last capture (its mock match) are still printed, and the replies in flight are still sent (for up to 65 s) before the process exits. |
| `--jsonl` | Print [one JSON object per line](#json-lines-output) per event, flushed at once; the banner and warnings go to stderr, so stdout holds only events. |
| `--workspace <DIR>` | The workspace whose `webhook/mocks.yaml` answers requests. Without it the workspace is searched from the current directory upwards; outside a workspace every request gets the default reply. |
| `--env <NAME>` | The environment whose secrets the [signature verifiers](#signature-verification) use (default: `RUSTMAN_ENV`, then `[defaults] default_environment`). |

The bind address can live in the workspace:

```toml
[webhook]
host = "127.0.0.1"   # the flags win over these
port = 9191
```

A port that cannot be bound stops the command with exit code 2:

```text
$ rustman webhook listen
error: webhook: cannot bind 127.0.0.1:9090: address in use
```

Binding a non-loopback address without `--allow-remote` is refused, and with it a warning stays on stderr:

```text
$ rustman webhook listen --host 0.0.0.0
error: webhook: binding 0.0.0.0 exposes the listener to the network; pass --allow-remote
$ rustman webhook listen --host 0.0.0.0 --allow-remote
Listening on http://0.0.0.0:9090
warning: the webhook listener on 0.0.0.0:9090 is reachable from the network
```

Ctrl+C stops the listener (exit code 130), even while nothing reads the output. When the output is closed (a pipe whose reader exited), the listener stops as soon as the next event cannot be printed, without waiting for replies in flight, and exits with code 0. Captures live in memory only; they are gone when the listener stops.

Printing never holds up the listener: when the output is read slower than events arrive (a paused `| less`, a slow `| jq`), requests are still answered, at most 256 events (32 MiB of bodies) wait to be printed, and the rest are dropped with `warning: N events not shown (output too slow)` on stderr once the output moves again. `rustman webhook listen` keeps no capture buffer of its own: it only prints events.

## HTTP handling and limits

The listener speaks HTTP/1.1 (including chunked bodies) and HTTP/2 over cleartext with prior knowledge, through `hyper`; nothing is parsed by hand. Each HTTP/1 connection carries one request: the response closes it, as most webhook senders expect.

Requests that break a rule are answered with an error, are **not** captured, and appear on the event log with the client address, the status and the reason:

| Rule | Default | Response |
|------|---------|----------|
| Malformed request line or head, conflicting `Content-Length` values, or `Transfer-Encoding` together with `Content-Length` | | `400` |
| Header fields per request | 100 | `431` (`too many header fields`) |
| Bytes of header fields per request | 64 KiB | `431` (`header fields too large`) |
| Body size, declared (`Content-Length`) or streamed | 10 MiB | `413`, before the body is read or as soon as the limit is passed; the connection is closed |
| Time to receive the request head, counted from the connection | 10 s | `408` when part of a head arrived; a connection that sent nothing (or speaks HTTP/2) is closed without a reply or a log line |
| Time to receive the body | 30 s | `408` |
| Idle connection (no request in flight, no traffic) | 60 s | the connection is closed |
| Connections served at once | 256 | more wait to be accepted (see the note below) |
| Requests in flight on one HTTP/2 connection | 16 | the sender waits (HTTP/2 flow control) |

```text
127.0.0.1:53120 rejected 431 too many header fields
```

Over HTTP/2 the protocol layer itself answers `431`, without a log line, to a header list over twice the header-byte limit plus 32 bytes per allowed field (about 131 KiB by default); smaller lists over the limits are refused and logged as above.

Bodies are held in memory while they are received, before the size check can drop them, so the requests in flight bound that memory: up to the body limit per request, which with the defaults is 2.5 GiB over HTTP/1 (256 connections) and in theory 40 GiB over HTTP/2 (16 streams on each). No overall budget caps it; lower `--max-body` on a machine where that matters.

The listener is a development tool, not a hardened server: a client that opens connections and sends nothing, or a head byte by byte, holds a connection slot until the 10 s head timeout. 256 such connections stall other deliveries for that long, since further connections wait in the operating system's backlog. Keep the listener on loopback, and use `--allow-remote` only on networks you trust.

Error responses are JSON, such as `{"error":"body larger than 10485760 bytes","status":413}`.

## Captures

Each capture keeps a sequence id, the receive time (RFC 3339 with the local offset), the client address, the HTTP version, the method, the path and the query string separately, every header field in arrival order with duplicates (names in the sender's case on HTTP/1; HTTP/2 names are lowercase by protocol), and the body as raw bytes. Nothing is decoded or replaced: a body that is not UTF-8 is kept byte for byte and shown as a hex dump; JSON bodies are shown pretty-printed and form bodies as fields.

Captures are kept in a ring buffer of at most 500 captures and 256 MiB of body bytes: when either bound would be passed, the oldest captures are dropped until both hold (the newest capture is always kept). The buffer's memory is therefore bounded by the byte budget plus one maximum body. Events (a capture included) are also queued for each front-end, up to 1024 of them: a front-end that falls that far behind can briefly keep evicted captures alive until it catches up, and then misses the oldest events. The TUI and the CLI read events as they arrive, so in practice the queue stays short.

## Responses and mocks

Without a matching mock the reply is:

```text
HTTP/1.1 200 OK
content-type: application/json
content-length: 34

{"status":"ok","source":"rustman"}
```

Mocks live in `webhook/mocks.yaml` in the workspace. The first mock whose method and path match answers; the request is captured either way.

```yaml
mocks:
  - method: POST              # or "*" for any method (the default)
    path: /hooks/github       # exact, or a glob: * within one path segment, ** across segments
    status: 500               # default 200
    headers: { X-Mock: "1" }
    body: '{"error":"boom"}'  # or body_file: github-error.json, inside mocks.yaml's directory (at most 10 MiB)
    delay_ms: 2000            # wait before answering; at most 60000
  - path: /hooks/**
    status: 202
```

The path is matched without the query string. A mock's reply always carries a correct `Content-Length`: a mock may not set `Content-Length`, `Transfer-Encoding`, `Host` or a hop-by-hop header (`Connection`, `Keep-Alive`, `Proxy-Connection`, `TE`, `Trailer`, `Upgrade`), and such a mocks file is refused. A `body_file` must be a relative path that stays inside the directory of `mocks.yaml` (symlinks included) and be at most 10 MiB.

Changes to any file under `webhook/` reload the mocks while the listener runs (after 200 ms of quiet); the next request uses them. A file that does not parse leaves the previous mocks active, and both outcomes are on the event log:

```text
mocks reloaded: 2 active
mocks not reloaded (previous mocks kept): /home/me/api/webhook/mocks.yaml: mock 1: delay_ms 90000 exceeds 60000
```

## Event log

One line per event:

| Event | Line |
|-------|------|
| Capture | `#1 POST /hooks/stripe?debug=1 from 127.0.0.1:53114 (2 B) -> 200` (the last number is the status answered), followed by ` signature: valid` (or `invalid`, `missing`, `expired`) when a verifier covers the path |
| Mock match | `#1 matched mock POST /hooks/github -> 500 after 2000 ms` |
| Rejection | `127.0.0.1:53120 rejected 413 body larger than 10485760 bytes` |
| Mock reload | `mocks reloaded: 2 active` or `mocks not reloaded (previous mocks kept): <error>` |

Printed lines are redacted with the verifier secrets: a sender that echoes a key back (in a path or query) shows `«redacted:<name>»` instead.

## JSON-lines output

With `--jsonl` every event is one JSON object on its own line, written and flushed as it happens, so a pipe sees it at once:

```text
$ rustman webhook listen --jsonl | jq -c '{path, signature}'
{"path":"/hooks/github","signature":"valid"}
```

Every object has a `type`:

| `type` | Fields |
|--------|--------|
| `capture` | `id`, `ts` (RFC 3339 with the local offset), `remote`, `version`, `method`, `path`, `query` (or `null`), `status` (the status answered), `mock` (the matching mock's path pattern, or `null`), `signature` (`valid`, `invalid`, `missing`, `expired`, or `null` without a verifier), `headers` (`[{"name": ..., "value": ...}]` in arrival order, duplicates included), `body_size`, and `body` when the body is UTF-8 text or `body_base64` when it is not |
| `rejection` | `remote`, `status` (400, 408, 413, 431), `reason` |
| `mock` | `id` (the capture), `method`, `pattern`, `status`, `delay_ms` |
| `mocks_reloaded` | `count` |
| `mocks_invalid` | `message` |

```json
{"type":"capture","id":1,"ts":"2026-11-18T10:15:30.125+01:00","remote":"127.0.0.1:53114","version":"HTTP/1.1","method":"POST","path":"/x","query":null,"status":200,"mock":null,"signature":null,"headers":[{"name":"Host","value":"127.0.0.1:9090"},{"name":"Content-Length","value":"2"}],"body_size":2,"body":"{}"}
```

Values are redacted like the human-readable lines: verifier secrets become `«redacted:<name>»`, and credential headers (`Authorization`, `Cookie`, ...) show a placeholder instead of their value.

## Signature verification

Verifiers in `rustman.toml` check the signature of every request to a path; the first verifier whose `path` matches (exact, or a glob with `*` within a segment and `**` across segments) decides. The check runs on the raw body bytes before anything is decoded, and MACs are compared in constant time. The verdict is part of the capture: `valid`, `invalid`, `missing` (no signature header) or, for Stripe, `expired`.

```toml
[[webhook.verifiers]]
path = "/hooks/github"
type = "github"                # X-Hub-Signature-256: sha256=<hex HMAC-SHA256 of the body>
secret = "{{gh_hook}}"

[[webhook.verifiers]]
path = "/hooks/stripe"
type = "stripe"                # Stripe-Signature: t=<unix time>,v1=<hex>[,v1=...]
secret = "{{stripe_whsec}}"
tolerance_s = 300              # optional; timestamps further off are "expired"

[[webhook.verifiers]]
path = "/hooks/shop/**"
type = "hmac"
header = "X-Signature"
algorithm = "sha256"           # sha256 (default), sha1 or sha512
encoding = "base64"            # hex (default) or base64
prefix = ""                    # text before the encoded MAC, e.g. "sha1="
secret = "{{shop_secret}}"
```

| Type | Signed content | Header |
|------|----------------|--------|
| `github` | the raw body, HMAC-SHA256 | `X-Hub-Signature-256: sha256=<hex>` |
| `stripe` | `<t>.<raw body>`, HMAC-SHA256; any `v1` may match (key rotation); `v0` is ignored | `Stripe-Signature: t=...,v1=...` |
| `hmac` | the raw body, with `algorithm` | `header`: `prefix` + the MAC in `encoding` |

A Stripe event whose signature matches but whose `t` is more than `tolerance_s` seconds old (300 by default), or more than that far in the future, is `expired`; a signature that does not match is `invalid` whatever its age.

`secret` must be a `{{variable}}` reference: keys stay out of `rustman.toml`, which is meant for Git. The variable is read from the active environment (`--env`, else `RUSTMAN_ENV`, else the workspace default; in the TUI, the environment shown in the status bar), normally as a secret (`secrets: [gh_hook]` in `environments/local.yaml` and `rustman secret set gh_hook --env local`), or as a plain variable of that environment; never from `[variables]` in `rustman.toml`. A secret that is not set stops the listener before it binds:

```text
error: rustman.toml: webhook verifier 1 (/hooks/github): gh_hook: secret not set; run 'rustman secret set gh_hook --env local'
```

Secret values never enter a capture, the event log, saved files or history.

## Replay and save

A capture can be replayed to any URL, saved as a request file or a history entry, or copied as a curl command. `rustman_webhook::replay`, `rustman_webhook::save` and `rustman_webhook::curl` provide these to every front-end; the [TUI webhook pane](tui.md#webhook-pane) exposes them as actions (`r`, `w`, `h`, `y`).

Both replay and request files use one header filter, so a capture whose body was edited still replays with correct framing (the Ghostman B1 bug). It removes:

- `Host`, `Content-Length` and `Transfer-Encoding`, which `rustman` computes for the new target and body;
- the hop-by-hop headers `Connection`, `Keep-Alive`, `Proxy-Connection`, `TE`, `Trailer` and `Upgrade`;
- every header named in a `Connection` header (`Connection: close, X-Trace` drops `X-Trace`).

Every other header is sent in its captured order, duplicates included.

**Replay** sends the original method, the body bytes (binary bodies byte for byte) and the filtered headers through the same HTTP engine as `rustman send`, so `Host` becomes the target's (`https://staging.example.com/hook` is sent with `Host: staging.example.com`). A capture without `Content-Type` gets `text/plain; charset=utf-8` for a text body or `application/octet-stream` for a binary one. A header value that is not text cannot be replayed and is reported.

**Save as request file** writes a `kind: http` file targeting the URL you choose (which may use variables, such as `{{api}}/hooks/stripe`):

```yaml
version: 1
kind: http
name: POST /hooks/stripe
description: 'Saved from webhook capture #4 (POST /hooks/stripe?debug=1)'
method: POST
url: '{{api}}/hooks/stripe'
headers:
  - name: Content-Type
    value: application/json
  - name: Stripe-Signature
    value: t=1700000000,v1=5257a869...
body:
  type: json
  content: '{"id":"evt_1","type":"invoice.paid"}'
```

JSON bodies are kept as JSON text, other text bodies as `type: text`, and a body that is not UTF-8 is written byte for byte to a file next to the request (`stripe.body.bin`) that the request sends with `type: file`. Any `{{` in captured text is escaped (`\{{`) so it is sent literally. Credentials (`Authorization`, `Proxy-Authorization`, `Cookie`, `Set-Cookie`) are left out and reported, since request files are meant for Git, and a capture holding a verifier secret is refused. Existing files are never replaced.

**Save to history** records the capture as an inbound entry in `.rustman/history/` (request `webhook`): the method, the path and query, the headers and the text body as received, redacted like every history entry, the status answered, and under `record.inbound` the capture id, receive time, client address, HTTP version, mock, signature verdict and, for a binary body, `body_base64`. `rustman history list` and `show` display these entries; they are not replayable from history (replay the capture instead).
