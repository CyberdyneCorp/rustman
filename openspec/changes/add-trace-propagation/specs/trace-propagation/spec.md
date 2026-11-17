## Purpose

Connects every Rustman exchange to the backend's distributed traces. Rustman injects standard trace context, records trace ids, links each exchange to the user's tracing UI, and can optionally export its own client spans, all off by default and local-first.

## ADDED Requirements

### Requirement: Off by default
Trace propagation SHALL be disabled unless enabled in an environment's `trace:` block, in `rustman.toml [trace]`, or with `--trace` on a command. With propagation disabled, Rustman SHALL NOT add any tracing header.

#### Scenario: Default request
- **GIVEN** no trace configuration
- **WHEN** a request is sent
- **THEN** it SHALL carry no `traceparent`, `tracestate`, `baggage` or B3 header

### Requirement: W3C traceparent injection
When enabled, each outgoing request SHALL carry a new `traceparent` header (`00-<32 hex trace id>-<16 hex span id>-<flags>`), with random non-zero ids and flags `01` when `sampled: true` (the default when enabled) or `00` otherwise.

#### Scenario: Header format
- **GIVEN** the environment `staging` has `trace: {enabled: true}`
- **WHEN** a request is sent
- **THEN** `traceparent` SHALL match `^00-[0-9a-f]{32}-[0-9a-f]{16}-01$`, and neither id SHALL be all zeros

#### Scenario: Unsampled
- **GIVEN** `trace: {enabled: true, sampled: false}`
- **WHEN** a request is sent
- **THEN** the `traceparent` flags SHALL be `00`

### Requirement: Trace id scope
By default, each exchange SHALL get its own trace id. With `trace: {scope: run}`, all exchanges in one `rustman run`, exploration or bench iteration SHALL share one trace id, each with its own span id, so a whole flow appears as one trace.

#### Scenario: Run-scoped trace
- **GIVEN** `trace: {enabled: true, scope: run}`
- **WHEN** a run executes `login`, `list`, `pay`
- **THEN** the three requests SHALL share the same trace id and have three distinct span ids

### Requirement: User-supplied trace headers win
If a request file or `--header` already sets `traceparent` (or a B3 header), Rustman SHALL send it unchanged and record its trace id instead of generating one.

#### Scenario: Explicit traceparent
- **GIVEN** a request with header `traceparent: 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01`
- **WHEN** it is sent with tracing enabled
- **THEN** that exact header SHALL be sent and the exchange SHALL record trace id `4bf92f3577b34da6a3ce929d0e0e4736`

### Requirement: Additional propagation formats
`trace.formats` SHALL accept any combination of `w3c` (default), `b3` (single `b3` header) and `b3multi` (`X-B3-TraceId`, `X-B3-SpanId`, `X-B3-Sampled`). All emitted formats SHALL carry the same ids. `trace.tracestate` and `trace.baggage` (key/value maps, with variables resolved) SHALL add `tracestate` and `baggage` headers.

#### Scenario: Legacy B3
- **GIVEN** `trace: {enabled: true, formats: [w3c, b3multi]}`
- **WHEN** a request is sent
- **THEN** `traceparent` and `X-B3-TraceId` SHALL carry the same trace id

#### Scenario: Baggage with a variable
- **GIVEN** `trace: {enabled: true, baggage: {tenant: "{{tenant_id}}"}}` and `tenant_id=acme`
- **WHEN** a request is sent
- **THEN** the request SHALL carry `baggage: tenant=acme`

### Requirement: Protocol coverage
Injection SHALL apply to every HTTP-based request kind present in the build: `http` and `graphql` (including the WebSocket upgrade request for subscriptions), `jsonrpc` over HTTP and WebSocket, and `grpc` as request metadata. Kinds without an HTTP layer SHALL be skipped silently.

#### Scenario: GraphQL request
- **GIVEN** tracing is enabled
- **WHEN** a `kind: graphql` query is sent over HTTP
- **THEN** the POST SHALL carry `traceparent`

### Requirement: Exchanges record trace ids
Every exchange sent with tracing enabled, or carrying a user-supplied trace header, SHALL record `trace.trace_id`, `trace.span_id` and `trace.sampled` in history and in run, exploration and bench reports. These fields SHALL be additive and optional.

#### Scenario: Run report
- **GIVEN** tracing is enabled
- **WHEN** `rustman run . --reporter json` completes
- **THEN** each exchange in the report SHALL contain a `trace` object with `trace_id` and `span_id`

### Requirement: Trace link templates
`[trace].link` (or an environment's `trace.link`) SHALL be a URL template using `{trace_id}`, `{span_id}`, `{start_ms}`, `{end_ms}` and any environment variable. Presets `jaeger`, `tempo`, `honeycomb` and `datadog` SHALL expand to documented templates that need a base URL (`trace.link_base`). An invalid template SHALL be reported when the workspace loads.

#### Scenario: Jaeger preset
- **GIVEN** `trace: {enabled: true, link: jaeger, link_base: "http://localhost:16686"}`
- **WHEN** an exchange with trace id `4bf9…4736` completes
- **THEN** its link SHALL be `http://localhost:16686/trace/4bf9…4736`

#### Scenario: Unknown placeholder
- **GIVEN** `link: "https://x/{traceid}"`
- **WHEN** the workspace loads
- **THEN** loading SHALL report `trace.link: unknown placeholder {traceid}`

### Requirement: Showing trace ids
The TUI response view SHALL show the trace id, with the link when configured, and `Ctrl+O` SHALL open the link in the system browser. `rustman send` SHALL print `trace: <trace_id>` and the link on stderr. `rustman trace open <exchange-id>` SHALL open the link of a history exchange, or exit with code 2 when no link is configured.

#### Scenario: CLI output
- **GIVEN** tracing is enabled with the `tempo` preset
- **WHEN** the user runs `rustman send get-user.rman.yaml`
- **THEN** stderr SHALL contain `trace: <32 hex>` and the Tempo URL

#### Scenario: No link configured
- **WHEN** the user runs `rustman trace open 42` and no link template is configured
- **THEN** the command SHALL exit with code 2 and `trace: no link template configured ([trace].link)`

### Requirement: Server timing and traceresponse
Rustman SHALL parse response `Server-Timing` headers (RFC-style `name;dur=<ms>;desc="…"`) and the W3C `traceresponse` header. Server timings SHALL appear as labeled sub-bars in the TUI timing tab and in report exchanges. A `traceresponse` whose trace id differs from the request's SHALL be recorded as the backend trace id and used for links.

#### Scenario: Server-Timing shown
- **GIVEN** a response with `Server-Timing: db;dur=53.2, app;dur=12`
- **WHEN** the user opens the Timing tab
- **THEN** the tab SHALL show `db 53.2 ms` and `app 12 ms` under the waiting phase

#### Scenario: Backend restarts the trace
- **GIVEN** the backend ignores the incoming context and returns `traceresponse: 00-<new id>-<span>-01`
- **WHEN** the exchange completes
- **THEN** the link SHALL use `<new id>` and both ids SHALL be recorded

### Requirement: OTLP export is opt-in and explicit
Rustman SHALL export spans only when `[trace.export]` sets `enabled = true` and an explicit `endpoint`. There SHALL be no default endpoint and no use of `OTEL_EXPORTER_OTLP_*` environment variables unless `use_env = true`. `protocol` SHALL be `grpc` (default) or `http/protobuf`.

#### Scenario: Endpoint required
- **GIVEN** `[trace.export] enabled = true` without `endpoint`
- **WHEN** the workspace loads
- **THEN** loading SHALL fail with `trace.export.endpoint is required when export is enabled`

#### Scenario: Env vars ignored by default
- **GIVEN** `OTEL_EXPORTER_OTLP_ENDPOINT` is set and export is not enabled
- **WHEN** a request is sent
- **THEN** no span SHALL be exported

### Requirement: Exported client spans
When export is enabled, each exchange SHALL produce one client span (kind `CLIENT`) with the exchange's trace and span ids, and OpenTelemetry HTTP semantic-convention attributes (method, URL with query values redacted, status code, server address), with phase timings as span events. Header values SHALL NOT be exported.

#### Scenario: Span attributes
- **GIVEN** export is enabled to a local test collector
- **WHEN** `GET {{base_url}}/users?token=abc` returns 200
- **THEN** the collector SHALL receive a CLIENT span with `http.request.method=GET`, `http.response.status_code=200`, and a URL whose `token` value is `«redacted:token»`

### Requirement: Export failures never affect requests
Span export SHALL be asynchronous and batched, and SHALL be bounded (queue of 2048 spans, dropping the oldest). Collector errors SHALL be logged once per minute as warnings, and SHALL NOT change exchange results, run outcomes or exit codes.

#### Scenario: Collector down
- **GIVEN** export is enabled and the collector is unreachable
- **WHEN** a run of 10 requests executes
- **THEN** all 10 requests SHALL complete normally, the exit code SHALL reflect only assertions, and one warning SHALL be shown

### Requirement: Redaction of trace data
Values in `tracestate` and `baggage` whose keys are declared secrets SHALL be redacted in history, reports and exported spans, following the `environments` rules. Trace ids and span ids are not secrets and SHALL NOT be redacted.

#### Scenario: Secret baggage
- **GIVEN** `baggage: {api_key: "{{api_key}}"}` where `api_key` is a secret
- **WHEN** the exchange is shown in history
- **THEN** the baggage SHALL show `api_key=«redacted:api_key»`
