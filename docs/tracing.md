# Trace propagation

Rustman can send standard trace context with every request, so the exchange shows up in the backend's distributed traces (Jaeger, Grafana Tempo, Honeycomb, Datadog, or any OpenTelemetry-compatible system). It is off by default: with no configuration, no tracing header is ever added, and nothing is sent anywhere but the request's own target.

## Turning it on

Propagation is enabled by one of:

- a `trace:` block in an environment file (`environments/<name>.yaml`),
- `[trace]` in `rustman.toml`,
- the `--trace` flag of `rustman send`, `rustman run`, `rustman graphql query`, `rustman rpc call` / `batch` / `discover` and `rustman grpc call`, which turns it on even when no block enables it (`enabled: false` included).

```yaml
# environments/staging.yaml
variables:
  base_url: https://staging.example.com
  tenant_id: acme
trace:
  enabled: true
```

```toml
# rustman.toml
[workspace]
name = "shop"
version = 1

[trace]
enabled = true
sampled = true
formats = ["w3c", "b3multi"]

[trace.baggage]
tenant = "{{tenant_id}}"
```

The active environment's block applies over `[trace]` key by key: a key the environment sets wins, the others come from `rustman.toml`. So `rustman.toml` can hold the shared formats and link while each environment only says `enabled: true`. A preset `link` may take its `link_base` from the other file; the merged result is checked for every environment when the workspace loads. `export` is a single key: an environment's `export:` table replaces `[trace.export]` as a whole, so each table that enables export names its own `endpoint`.

## Settings

| Key | Default | Meaning |
|-----|---------|---------|
| `enabled` | `false` | Add trace headers to every request. |
| `sampled` | `true` | The sampled flag sent: `traceparent` flags `01`, or `00` when `false`; `1`/`0` in B3. |
| `scope` | `exchange` | `exchange`: each request starts its own trace. `run`: every request of one `rustman run` (or one exploration) shares one trace id, each with its own span id, so a whole flow appears as one trace. |
| `formats` | `[w3c]` | Any of `w3c` (`traceparent`), `b3` (the single `b3` header) and `b3multi` (`X-B3-TraceId`, `X-B3-SpanId`, `X-B3-Sampled`). Every format carries the same ids. |
| `tracestate` | none | A map of `tracestate` members. Values may use `{{variables}}`. |
| `baggage` | none | A map of `baggage` members. Values may use `{{variables}}`; they are percent-encoded on the wire. |
| `link` | none | A trace link template, or a preset name (see below). |
| `link_base` | none | The base URL of a preset link, such as `http://localhost:16686`. |

Every key is checked when the workspace loads; an unknown key or format is an error, and so is an invalid `tracestate` or `baggage` key. In `rustman.toml`, the `[trace]` keys above are known and never warn as `unknown key`.

### Variables in `tracestate` and `baggage`

Values are resolved once per run from `--var` overrides, the active environment (secrets included) and `rustman.toml [variables]`. Request variables and captures are not available there. An unresolved name stops the run before anything is sent, with `trace: unresolved variable(s): <name>`, and a resolved `tracestate` value that is not valid there (a comma, `=`, or a character outside printable ASCII) fails with `trace.tracestate: invalid tracestate value for '<key>'`.

```yaml
trace:
  enabled: true
  baggage:
    tenant: "{{tenant_id}}"
```

With `tenant_id=acme`, every request carries `baggage: tenant=acme`. A `baggage` header over the W3C limits (64 members or 8192 bytes) is still sent, with a warning, since servers may drop or reject it.

A secret used in a `tracestate` or `baggage` value is sent to the server as written (that is the point of baggage), and is redacted like any other secret in every output Rustman writes, in its percent-encoded `baggage` form too. Its headers are dropped on a cross-origin redirect, like any other header carrying a secret.

## What is sent

With `trace: {enabled: true}`, each request carries a new `traceparent`:

```text
traceparent: 00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01
```

Trace and span ids are random (from the operating system's CSPRNG through `rand`) and never all zeros.

### Headers you set yourself win

If a request file, a collection's default headers or `--header` already sets `traceparent`, `b3` or the `X-B3-*` headers, Rustman sends them unchanged and uses their ids instead of generating new ones. Formats the request does not set are added with those same ids. A `tracestate` or `baggage` header you set is never replaced either. A `traceparent`, `b3` or `X-B3-TraceId`/`X-B3-SpanId` header you set that is not valid (uppercase hex, version `ff`, or not a trace context at all) is also sent unchanged, but Rustman then adds no trace header, records no trace id or link, and prints a warning, so the request never carries two conflicting traces. A B3 sampling decision without ids (`b3: 0`, `b3: 1`, `b3: d`, or `X-B3-Sampled`/`X-B3-Flags` without `X-B3-TraceId` and `X-B3-SpanId`) is valid B3: it is sent unchanged with no warning, and Rustman adds no trace header, so a deny decision is never overridden.

```yaml
headers:
  - name: traceparent
    value: 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01
```

### Signed requests

The headers are added before auth helpers sign the request, so an AWS SigV4 signature covers `traceparent` and the other trace headers.

### CORS

Trace headers are custom request headers: a browser-facing API with a strict CORS policy may reject them in a preflight. That only matters when you replay Rustman's requests from a browser; Rustman itself sends no preflight.

## Request kinds

| Kind | Where the context goes |
|------|------------------------|
| `http` | The request's headers, on every redirect hop. |
| `graphql` | The HTTP `POST` or `GET`, the SSE subscription request, and the WebSocket upgrade request. |
| `jsonrpc` | The HTTP request, or the WebSocket upgrade. A WebSocket connection is reused for the run, so calls sharing it share the handshake's context. |
| `grpc` | Request metadata (`traceparent`, `b3`, `x-b3-*`) on native gRPC; HTTP headers on gRPC-Web. |
| `ws` | The WebSocket upgrade request. Frames carry no trace context, so the whole session shares the handshake's. |
| `sse` | The stream request, sent again on each reconnect. |
| `mcp` | Every HTTP request of a Streamable HTTP or legacy HTTP+SSE server's session. A stdio server has no HTTP layer and gets nothing. |

## What is recorded

Every exchange that carried trace context (propagation on, or a `traceparent`/B3 header the request set itself, even with propagation off) records a `trace` object in [history](history.md) and in each request of a `--reporter json` run report (`requests[].trace`), and exploration findings record the ids of their evidence exchange (`evidence.trace`). Sessions of the `ws` and `sse` kinds, WebSocket calls of `graphql` and `jsonrpc`, MCP sessions and native gRPC calls carry the context but do not record it yet:

```json
"trace": {
  "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
  "span_id": "00f067aa0ba902b7",
  "sampled": true,
  "backend_trace_id": "0af7651916cd43dd8448eb211c80319c",
  "baggage": "tenant=acme,api_key=«redacted:api_key»",
  "start_ms": 1763372400000,
  "end_ms": 1763372400084,
  "link": "http://localhost:16686/trace/0af7651916cd43dd8448eb211c80319c"
}
```

Every field is optional in the schema except the ids and `sampled`, so reports and history written before tracing existed stay valid (`rustman.report/v1` and `rustman.history/v1` are unchanged). Trace and span ids are not secrets and are never redacted; `tracestate`, `baggage` and the link are [redacted](environments.md#redaction) like any other output, so a secret baggage value shows as `api_key=«redacted:api_key»`.

### `traceresponse` and `Server-Timing`

A response's W3C `traceresponse` header names the trace the server recorded. When its trace id differs from the one sent (the backend ignored the incoming context and started its own trace), it is recorded as `backend_trace_id`, both ids are kept, and links use the backend's id. A malformed `traceresponse` is ignored.

The response's `Server-Timing` metrics (`db;dur=53.2;desc="query", app;dur=12`) are parsed whether or not tracing is on and recorded per exchange as `exchange.server_timings` (`[{"name": "db", "dur_ms": 53.2, "desc": "query"}]`). The parser is lenient: it keeps each metric's first `dur` and `desc`, and skips a malformed metric (logged at `-vvv`) while keeping the others. The TUI's Timing tab draws them as sub-bars under the waiting phase.

## Links

`link` names where a trace can be opened. A preset expands to a template that starts with `link_base` (required with a preset, in the same file or the other one; loading fails with `trace.link: preset 'jaeger' needs trace.link_base` when neither sets it):

| Preset | Template |
|--------|----------|
| `jaeger` | `{base}/trace/{trace_id}` |
| `tempo` | `{base}/explore?left=…` (Grafana Explore with a TraceQL query for `{trace_id}`) |
| `honeycomb` | `{base}/trace?trace_id={trace_id}` |
| `datadog` | `{base}/apm/trace/{trace_id}` |

A custom template may use `{trace_id}`, `{span_id}`, `{start_ms}`, `{end_ms}` (Unix milliseconds of the exchange), `{trace_id_dd}` (the lower 64 bits of the trace id in decimal, for Datadog setups that show 64-bit ids, such as `https://app.datadoghq.com/apm/trace/{trace_id_dd}`) and `{{variables}}`, resolved like `baggage` values. Any other `{name}` is reported when the workspace loads:

```text
environments/staging.yaml:3: trace.link: unknown placeholder {traceid}
```

A link is opened in the system browser, so it must be an absolute `http://` or `https://` URL: a template that does not start with one fails when the workspace loads with `trace.link: expected an http:// or https:// URL, got '<link>'`, and one that starts with a `{{variable}}` (or a preset whose `link_base` is not such a URL) is checked once resolved. A link whose variable cannot be resolved, or that does not resolve to an `http(s)` URL, only loses the links (a warning is logged); requests are still sent. The browser opener itself also refuses anything but an `http(s)` URL.

## Opening a trace

- **CLI**: `rustman send` prints the ids and the link on stderr, after the response:

  ```text
  trace: 4bf92f3577b34da6a3ce929d0e0e4736
  trace: http://localhost:16686/trace/4bf92f3577b34da6a3ce929d0e0e4736
  ```

  A `trace: backend <id>` line appears when the server's `traceresponse` named another trace.
- `rustman trace open <exchange-id>` opens the link of a [history](history.md) exchange (its id or a unique prefix, as `rustman history show` takes) in the system browser and prints it on stdout; `--no-browser` only prints it. The link is built from the exchange's ids and the link template configured now for the exchange's environment. Without a template it exits with code 2 and `trace: no link template configured ([trace].link)`; an exchange that carried no trace context also exits with 2.
- **TUI**: the Timing tab shows the trace id, the backend's id and the link, and `Ctrl+O` (action `trace.open`) opens the link in the system browser.

The browser is opened with `open` (macOS), `rundll32 url.dll,FileProtocolHandler` (Windows) or `xdg-open` (Linux and the BSDs). The TUI shows the recorded (redacted) link; when it hides a secret variable, `Ctrl+O` rebuilds the real link from the template configured now and the active environment's secrets, as `rustman trace open` does, and hands only that to the browser.

## Exporting client spans (OTLP)

Rustman can also send its own side of each exchange to an OpenTelemetry collector, so a trace shows the client span next to the backend's server spans. Export is a build feature and an explicit setting:

- **Build**: the default `rustman` binary has no exporter and no OpenTelemetry crate. Build with `cargo install --path crates/rustman-cli --features otlp` (or `cargo build -p rustman-cli --features otlp`). A build without it that finds export enabled prints `warning: trace.export: this build cannot export spans (build rustman with the `otlp` feature)` and sends requests as usual.
- **Setting**: `[trace.export]` in `rustman.toml` (or `export:` in an environment's `trace:` block, which replaces the whole table):

```toml
[trace.export]
enabled = true
endpoint = "http://localhost:4317"
protocol = "grpc"          # or "http/protobuf"
service_name = "api-tests" # the spans' service.name; "rustman" by default
use_env = false
```

There is no default endpoint: `enabled = true` without one fails when the workspace loads with `trace.export.endpoint is required when export is enabled`. Enabling export also turns trace propagation on (exported spans need the ids the requests carried), as `enabled = true` in `[trace]` would.

| Key | Meaning |
|-----|---------|
| `enabled` | Export spans. Off by default. |
| `endpoint` | `http://` or `https://` collector address. With `grpc`, spans go to the `opentelemetry.proto.collector.trace.v1.TraceService/Export` method (HTTP/2, h2c for `http://`); with `http/protobuf`, to `<endpoint>/v1/traces`, or to the endpoint as written when it has a path of its own. |
| `protocol` | `grpc` (default) or `http/protobuf`. |
| `service_name` | The `service.name` resource attribute. |
| `use_env` | Read the standard variables `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_(TRACES_)PROTOCOL` and `OTEL_EXPORTER_OTLP_(TRACES_)HEADERS` (`key=value,key2=value2`, for an API key); they take precedence over the file. Off by default: an `OTEL_EXPORTER_OTLP_*` variable set in your shell is never read otherwise, so a shared collector configured for other tools never receives Rustman's spans by accident. |

The exporter uses Rustman's own HTTP stack (rustls with the platform's trust store; no proxy), so it needs no system library.

### What a span holds

One span of kind `CLIENT` per exchange that got a response and carried a sampled trace context, with the exchange's trace and span ids (so the backend's server span is its child) and its `tracestate` (redacted). Its name is the HTTP method, and it has the OpenTelemetry HTTP semantic-convention attributes:

| Attribute | Example |
|-----------|---------|
| `http.request.method` | `GET` |
| `url.full` | `https://api.example.com/users?token=«redacted:token»` |
| `url.scheme`, `server.address`, `server.port` | `https`, `api.example.com`, `443` |
| `http.response.status_code` | `200` |
| `network.protocol.version` | `1.1` |
| `error.type` | the status code, for `4xx` and `5xx` (the span status is then `ERROR`) |
| `rustman.request.id` | the request's id in the workspace; for an exploration probe, `<probe id>/<step>` (`cleanup` for a cleanup delete) |

Every query value in `url.full` is replaced by `«redacted:<key>»` and every query component without `=` by `«redacted»`, credentials in the URL become `REDACTED:REDACTED` and the fragment is dropped. **Header values are never exported**, so `Authorization` and cookies cannot leak this way. The phases (`dns`, `connect`, `tls`, `waiting`, `download`) are span events at the end of each phase, with `rustman.phase.duration_ms`. Request and response bodies are not exported.

### Delivery and failures

Spans are built after an exchange completes and queued; a worker thread sends them in batches every 5 seconds (or as soon as 512 are waiting), so export never sits on a request's path. The queue holds 2048 spans and drops the oldest when full. `rustman send`, `rustman run` and the other commands that send requests wait up to 5 seconds for the last batch before exiting; the TUI does the same when it quits, and prints the last export warnings once the terminal is restored.

Explorations export too: `rustman explore run` (and `explore resume`) and the agent's explore tools send one client span per probe exchange (each step of a probe, and each cleanup delete) through the same exporter, with the `[trace.export]` of the exploration's environment. Under `scope: run` every probe of an exploration shares one trace. `rustman explore run` waits up to 5 seconds for the last batch before exiting and prints export warnings like `rustman run`; the agent server, which outlives the exploration, lets the worker send them on its schedule.

A collector that is down, slow or refusing never changes a result: exchanges, assertions, reports and exit codes are the same as without export. Failures are shown as a warning at most once a minute, such as:

```text
warning: trace export to localhost:4317 failed: cannot connect to localhost:4317: Connection refused (os error 111) (3 span(s) dropped; requests are unaffected)
```

The TUI shows `OTLP →<host:port>` in the status bar while the active environment exports spans. Export failures appear there as warning toasts, after the next send.

## Library use

Front-ends and other transports use the same primitives from `rustman-core`'s `trace` module: `TraceConfig` (the settings block), `TracePropagation` (the settings resolved for a run, set on `SendOptions::trace` for `rustman-http`), and `TraceContextInjector`, the trait a transport implements to receive the headers or metadata when it does not go through `rustman-http`'s request builder, as native gRPC does. `ClientSpan::from_record` builds the exported span of a recorded exchange, and `ExportTarget` resolves `[trace.export]`; the exporter itself is `rustman-http`'s `otlp` feature, used through the runner, or through `runner::SpanExport` (`record_exchange`, `flush`, `take_warnings`) by senders that do not go through a runner, as explorations do.
