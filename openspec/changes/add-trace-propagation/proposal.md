# Proposal

## Why

When a request fails or is slow, the next question is always "what happened inside the backend?" Most backends already emit OpenTelemetry traces, but an API client's requests arrive with no trace context. That makes them hard to find in Jaeger, Tempo or Datadog. Injecting W3C Trace Context and linking every exchange to its trace turns Rustman into the front door of the observability stack, without sending any data anywhere by default.

## What Changes

- Inject a W3C `traceparent` (and `tracestate`) into outgoing requests, with optional `baggage` and B3 headers (single or multi) for older systems.
  - It applies to every HTTP-based request kind present in the build: `http`, `graphql`, and `jsonrpc` and `grpc` (as metadata) once their changes land.
- A per-environment toggle, propagation formats, and a sampled flag. Off by default.
- Each exchange stores its trace id and span id. The TUI and CLI show the trace id and a link built from a configured URL template, with presets for Jaeger, Grafana Tempo, Honeycomb and Datadog.
- Parse response `traceresponse` and `Server-Timing` headers and show them in the timing view and reports.
- Run, exploration and bench reports include the trace id per exchange.
- Optional export of Rustman's own client spans via OTLP (gRPC or HTTP/protobuf) to a collector. Opt-in, off by default, and only to an explicitly configured endpoint (local-first).
- A `rustman trace open <exchange-id>` command and a TUI key that open the trace link.

Out of scope: receiving or storing backend traces, rendering trace waterfalls in Rustman, metrics and log export, auto-instrumenting Rustman internals beyond the per-exchange client span, and vendor-specific APIs (Rustman only builds links).

## Capabilities

### New Capabilities
- `trace-propagation`: trace context injection, exchange-to-trace linking with URL templates, server timing parsing, trace ids in reports, and opt-in OTLP span export.

### Modified Capabilities
None. It reuses:
- `http-requests` (header injection point and timing data);
- `request-history` (the exchange record gains optional trace fields additively);
- `environments` (per-environment settings, redaction);
- `test-reports` (additive per-exchange `trace` object);
- `tui` (timing tab and key binding).

## Impact

- New module in `rustman-core` for trace context generation and parsing. Optional OTLP export uses the OTLP messages of `opentelemetry-proto`, sent over `rustman-http`'s own transport, behind the cargo feature `otlp`, which is off by default (the default build has no OpenTelemetry crate) and does nothing at runtime unless configured (design D1, D6).
- A new `trace:` block in environment files and `[trace]` in `rustman.toml`.
- The history exchange format and report exchanges gain optional `trace` fields (additive, no version bump).
