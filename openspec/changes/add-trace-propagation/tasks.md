# Tasks

## 1. Trace context primitives

- [ ] 1.1 Implement W3C `traceparent`/`tracestate` generation and parsing, B3 single/multi, and `baggage` encoding in `rustman-core`, with an injectable RNG. Verify: unit tests with W3C Trace Context and B3 spec test vectors, and a test that ids are never all zeros.
- [ ] 1.2 Add the `trace:` environment block and `[trace]` config (enabled, sampled, scope, formats, tracestate, baggage, link, link_base) with load-time validation. Verify: parser tests, including the unknown-placeholder error message.

## 2. Injection

- [ ] 2.1 Add the injection stage to the `rustman-http` pipeline after user headers: off by default, `--trace` flag, user-supplied headers win. Verify: integration tests against a local echo server, covering no headers by default, the regex-matching `traceparent` when enabled, and an explicit header sent unchanged.
- [ ] 2.2 Implement `scope: run` in the runner, exploration and bench contexts. Verify: a three-request run shares one trace id with distinct span ids.
- [ ] 2.3 Expose the `TraceContextInjector` trait for GraphQL (HTTP and WS upgrade), JSON-RPC and gRPC metadata, wired only for crates present in the build. Verify: a GraphQL integration test asserts `traceparent` on the POST. A compile test confirms injection is skipped for kinds without an HTTP layer.
- [ ] 2.4 Document propagation settings in `docs/tracing.md`. Verify: docs build.

## 3. Recording and display

- [ ] 3.1 Add optional `trace` fields to history exchanges and report exchanges (additive to `rustman.report/v1`). Verify: the report validates against `rustman report schema`, and old fixtures without `trace` still load.
- [ ] 3.2 Implement `Server-Timing` and `traceresponse` parsing, with the backend trace id used for links. Verify: unit tests for valid and malformed headers, and a test where a different `traceresponse` id becomes the link id.
- [ ] 3.3 Implement link templates and presets (jaeger, tempo, honeycomb, datadog with `{trace_id_dd}`). Verify: table-driven tests for expected URLs.
- [ ] 3.4 Show the trace id and link in the TUI response view, plus `Ctrl+O` to open it and server-timing sub-bars in the Timing tab. Verify: ratatui `TestBackend` snapshots, and a test with an injected opener asserts the URL.
- [ ] 3.5 Print the trace id and link on stderr in `rustman send`, and implement `rustman trace open <exchange-id>` (exit 2 without a template). Verify: CLI tests for stderr content and the exit code.
- [ ] 3.6 Apply redaction to secret `tracestate`/`baggage` values in history and reports. Verify: a test asserts `api_key=«redacted:api_key»`.

## 4. OTLP export

- [ ] 4.1 Add the `otlp` cargo feature with lazily initialized `opentelemetry-otlp` (grpc and http/protobuf). The endpoint is required when enabled, and the env vars are honored only with `use_env = true`. Verify: config validation tests, and a test where `OTEL_EXPORTER_OTLP_ENDPOINT` set without enablement exports nothing (checked on a test collector).
- [ ] 4.2 Build CLIENT spans after each exchange with HTTP semantic-convention attributes, redacted URL query values, phase events, and no headers. Verify: an in-process OTLP test collector asserts the attributes and redacted `token`.
- [ ] 4.3 Batch processor (queue 2048, drop-oldest, 5s delay), with collector failures as rate-limited warnings that never change results. Verify: a test with the collector down asserts 10 successful exchanges, an exit code driven only by assertions, and exactly one warning per minute.
- [ ] 4.4 Add an `OTLP →<host>` TUI status badge while exporting, and document export in `docs/tracing.md`. Verify: a snapshot test and docs build.

## 5. Integration

- [ ] 5.1 End-to-end: `rustman run` with `scope: run` against a local test API instrumented with OpenTelemetry, exporting to a Jaeger all-in-one container. Assert the API's server spans and Rustman's client spans share the trace id, and the printed Jaeger link resolves (HTTP 200). Verify: the CI job passes on Linux. macOS and Windows run the in-process collector variant.
