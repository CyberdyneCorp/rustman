# Tasks

See design.md "Building on existing code" for the components each task reuses.

## 1. Trace context primitives

- [x] 1.1 Implement W3C `traceparent`/`tracestate` generation and parsing, B3 single/multi, and `baggage` encoding in `rustman-core` (a `trace` module, on the workspace `rand`), with an injectable RNG. Verify: unit tests with W3C Trace Context and B3 spec test vectors, and a test that ids are never all zeros.
- [x] 1.2 Add the `trace:` block to environment files (`Environment`, `crates/rustman-core/src/environment.rs`) and `[trace]` to `Settings` (`crates/rustman-core/src/settings.rs`, so it no longer warns as an unknown key), with enabled, sampled, scope, formats, tracestate, baggage, link and link_base, validated at load. Verify: parser tests, including the unknown-placeholder error message and no unknown-key warning for `[trace]`.

## 2. Injection

- [x] 2.1 Inject in `HttpEngine::prepare` (`crates/rustman-http/src/engine.rs`) after `wire::build`, driven by a trace setting on `SendOptions`: off by default, `--trace` flag, user-supplied headers win. Verify: integration tests against a local echo server, covering no headers by default, the regex-matching `traceparent` when enabled, an explicit header sent unchanged, and a SigV4 request whose signature covers `traceparent`.
- [x] 2.2 Implement `scope: run` in the runner (`crates/rustman-http/src/runner`) and exploration contexts; the bench context follows when `add-load-testing` lands (design open question). Verify: a three-request run shares one trace id with distinct span ids.
- [x] 2.3 Cover the other kinds: GraphQL (HTTP, SSE, WebSocket upgrade) and JSON-RPC (HTTP, WebSocket) already pass through `prepare`, `open_stream` and `request_head`, so only tests are needed for them; expose the `TraceContextInjector` trait for transports outside `rustman-http`'s request builder (gRPC metadata). Verify: GraphQL integration tests assert `traceparent` on the POST and on the WebSocket upgrade, a JSON-RPC test asserts it on the HTTP call, and kinds without an HTTP layer send nothing.
- [x] 2.4 Document propagation settings in `docs/tracing.md`. Verify: docs build.
- [x] 2.5 Follow-up: inject into the WebSocket and SSE request kinds of `rustman-realtime` (`Request::Ws`/`Sse`); their handshakes go through `request_head`/`open_stream` with the runner's `SendOptions`, so this was tests only. Verify: `crates/rustman-realtime/tests/runner.rs` `trace_context_reaches_the_websocket_handshake_and_the_sse_request` asserts `traceparent` on the WebSocket upgrade and the SSE request, sharing the run's trace id with `scope = "run"`, and `realtime_requests_carry_no_trace_context_by_default`.

## 3. Recording and display

- [x] 3.1 Add optional `trace` fields to `RequestRecord` (`crates/rustman-core/src/run/`, hence history entries) and to report exchanges: hand-edit `crates/rustman-report/src/run.schema.json` additively and extend the `schemars` types of `crates/rustman-report/src/exploration.rs`, then regenerate `docs/schemas/report.schema.json`. Verify: the report validates against `rustman report schema`, and old history entries and report fixtures without `trace` (`crates/rustman-report/tests/fixtures/`) still load.
- [x] 3.2 Implement `Server-Timing` and `traceresponse` parsing, with the backend trace id used for links. Verify: unit tests for valid and malformed headers, and a test where a different `traceresponse` id becomes the link id.
- [x] 3.3 Implement link templates and presets (jaeger, tempo, honeycomb, datadog with `{trace_id_dd}`). Verify: table-driven tests for expected URLs.
- [x] 3.4 Show the trace id and link in the TUI response view, plus a new stable action id (e.g. `trace.open`) in `crates/rustman-tui/src/keymap/actions.rs` bound to `Ctrl+O` in `keymap/mod.rs`, and server-timing sub-bars in the Timing tab (`crates/rustman-tui/src/response/timing.rs`). Verify: ratatui `TestBackend` snapshots, and a test with an injected opener asserts the URL.
- [x] 3.5 Print the trace id and link on stderr in `rustman send`, and implement `rustman trace open <exchange-id>` resolving ids and unique prefixes with the history store as `rustman history show` does (exit 2 without a template); move the duplicated system-browser opener (`rustman_mcp::client::oauth::open_browser`, `commands/exec.rs`) to one shared place. Verify: CLI tests for stderr content and the exit code.
- [x] 3.6 Apply redaction to secret `tracestate`/`baggage` values in history and reports. Verify: a test asserts `api_key=«redacted:api_key»`.
- [ ] 3.7 Follow-up: record trace ids for exchanges without an HTTP `Exchange` (GraphQL and JSON-RPC over WebSocket, where calls share the handshake's context, the MCP client's HTTP session and native gRPC metadata) and for the realtime kinds once `rustman-realtime` is on main (design open question on WebSocket calls). Verify: run reports of those kinds carry `trace` with the handshake's ids.

## 4. OTLP export

- [x] 4.1 Add the `otlp` cargo feature with a lazily initialized OTLP exporter (grpc and http/protobuf; `opentelemetry-proto` messages over Rustman's own transport, per design D1 and its open question on the client). The endpoint is required when enabled, and the env vars are honored only with `use_env = true`. Verify: config validation tests, and a test where `OTEL_EXPORTER_OTLP_ENDPOINT` set without enablement exports nothing (checked on a test collector).
- [x] 4.2 Build CLIENT spans after each exchange with HTTP semantic-convention attributes, redacted URL query values, phase events, and no headers. Verify: an in-process OTLP test collector asserts the attributes and redacted `token`.
- [x] 4.3 Batch processor (queue 2048, drop-oldest, 5s delay), with collector failures as rate-limited warnings that never change results. Verify: a test with the collector down asserts 10 successful exchanges, an exit code driven only by assertions, and exactly one warning per minute.
- [x] 4.4 Add an `OTLP →<host>` TUI status badge while exporting, and document export in `docs/tracing.md`. Verify: a snapshot test and docs build.
- [ ] 4.5 Follow-up: export the client spans of exploration probes (`rustman explore run` and the agent's explore tools), which propagate trace context but bypass the runner's span export; until then `rustman explore run` warns when export is enabled. Verify: an exploration against the in-process collector exports one CLIENT span per probe exchange.

## 5. Integration

- [ ] 5.1 End-to-end: `rustman run` with `scope: run` against a local test API instrumented with OpenTelemetry, exporting to a Jaeger all-in-one container. Assert the API's server spans and Rustman's client spans share the trace id, and the printed Jaeger link points at that trace (checked through Jaeger's `/api/traces/<id>`: the UI is a single-page app that answers 200 for any `/trace/<id>`, so a status check proves nothing). Verify: the CI job passes on Linux. macOS and Windows run the in-process collector variant. Status: the `trace-jaeger` CI job (`.github/workflows/ci.yml`, Jaeger all-in-one and HotROD 1.60.0 as service containers, test `jaeger_shows_rustman_and_the_api_in_one_trace` in `crates/rustman-cli/tests/otlp.rs`) and the in-process variant (`a_run_scoped_trace_reaches_the_server_and_the_collector`, run on every OS) are in place; the in-process variant passes locally, the Jaeger job has not run yet (no container runtime locally), so this stays open until CI passes.
