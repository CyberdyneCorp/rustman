# Tasks

See design.md "Building on existing code" for the components each task reuses.

## 1. Trace context primitives

- [ ] 1.1 Implement W3C `traceparent`/`tracestate` generation and parsing, B3 single/multi, and `baggage` encoding in `rustman-core` (a `trace` module, on the workspace `rand`), with an injectable RNG. Verify: unit tests with W3C Trace Context and B3 spec test vectors, and a test that ids are never all zeros.
- [ ] 1.2 Add the `trace:` block to environment files (`Environment`, `crates/rustman-core/src/environment.rs`) and `[trace]` to `Settings` (`crates/rustman-core/src/settings.rs`, so it no longer warns as an unknown key), with enabled, sampled, scope, formats, tracestate, baggage, link and link_base, validated at load. Verify: parser tests, including the unknown-placeholder error message and no unknown-key warning for `[trace]`.

## 2. Injection

- [ ] 2.1 Inject in `HttpEngine::prepare` (`crates/rustman-http/src/engine.rs`) after `wire::build`, driven by a trace setting on `SendOptions`: off by default, `--trace` flag, user-supplied headers win. Verify: integration tests against a local echo server, covering no headers by default, the regex-matching `traceparent` when enabled, an explicit header sent unchanged, and a SigV4 request whose signature covers `traceparent`.
- [ ] 2.2 Implement `scope: run` in the runner (`crates/rustman-http/src/runner`) and exploration contexts; the bench context follows when `add-load-testing` lands (design open question). Verify: a three-request run shares one trace id with distinct span ids.
- [ ] 2.3 Cover the other kinds: GraphQL (HTTP, SSE, WebSocket upgrade) and JSON-RPC (HTTP, WebSocket) already pass through `prepare`, `open_stream` and `request_head`, so only tests are needed for them; expose the `TraceContextInjector` trait for transports outside `rustman-http`'s request builder (gRPC metadata). Verify: GraphQL integration tests assert `traceparent` on the POST and on the WebSocket upgrade, a JSON-RPC test asserts it on the HTTP call, and kinds without an HTTP layer send nothing.
- [ ] 2.4 Document propagation settings in `docs/tracing.md`. Verify: docs build.

## 3. Recording and display

- [ ] 3.1 Add optional `trace` fields to `RequestRecord` (`crates/rustman-core/src/run/`, hence history entries) and to report exchanges: hand-edit `crates/rustman-report/src/run.schema.json` additively and extend the `schemars` types of `crates/rustman-report/src/exploration.rs`, then regenerate `docs/schemas/report.schema.json`. Verify: the report validates against `rustman report schema`, and old history entries and report fixtures without `trace` (`crates/rustman-report/tests/fixtures/`) still load.
- [ ] 3.2 Implement `Server-Timing` and `traceresponse` parsing, with the backend trace id used for links. Verify: unit tests for valid and malformed headers, and a test where a different `traceresponse` id becomes the link id.
- [ ] 3.3 Implement link templates and presets (jaeger, tempo, honeycomb, datadog with `{trace_id_dd}`). Verify: table-driven tests for expected URLs.
- [ ] 3.4 Show the trace id and link in the TUI response view, plus a new stable action id (e.g. `trace.open`) in `crates/rustman-tui/src/keymap/actions.rs` bound to `Ctrl+O` in `keymap/mod.rs`, and server-timing sub-bars in the Timing tab (`crates/rustman-tui/src/response/timing.rs`). Verify: ratatui `TestBackend` snapshots, and a test with an injected opener asserts the URL.
- [ ] 3.5 Print the trace id and link on stderr in `rustman send`, and implement `rustman trace open <exchange-id>` resolving ids and unique prefixes with the history store as `rustman history show` does (exit 2 without a template); move the duplicated system-browser opener (`rustman_mcp::client::oauth::open_browser`, `commands/exec.rs`) to one shared place. Verify: CLI tests for stderr content and the exit code.
- [ ] 3.6 Apply redaction to secret `tracestate`/`baggage` values in history and reports. Verify: a test asserts `api_key=«redacted:api_key»`.

## 4. OTLP export

- [ ] 4.1 Add the `otlp` cargo feature with lazily initialized `opentelemetry-otlp` (grpc and http/protobuf, client per the design open question). The endpoint is required when enabled, and the env vars are honored only with `use_env = true`. Verify: config validation tests, and a test where `OTEL_EXPORTER_OTLP_ENDPOINT` set without enablement exports nothing (checked on a test collector).
- [ ] 4.2 Build CLIENT spans after each exchange with HTTP semantic-convention attributes, redacted URL query values, phase events, and no headers. Verify: an in-process OTLP test collector asserts the attributes and redacted `token`.
- [ ] 4.3 Batch processor (queue 2048, drop-oldest, 5s delay), with collector failures as rate-limited warnings that never change results. Verify: a test with the collector down asserts 10 successful exchanges, an exit code driven only by assertions, and exactly one warning per minute.
- [ ] 4.4 Add an `OTLP →<host>` TUI status badge while exporting, and document export in `docs/tracing.md`. Verify: a snapshot test and docs build.

## 5. Integration

- [ ] 5.1 End-to-end: `rustman run` with `scope: run` against a local test API instrumented with OpenTelemetry, exporting to a Jaeger all-in-one container. Assert the API's server spans and Rustman's client spans share the trace id, and the printed Jaeger link resolves (HTTP 200). Verify: the CI job passes on Linux. macOS and Windows run the in-process collector variant.
