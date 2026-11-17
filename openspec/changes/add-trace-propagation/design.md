# Design

## Context

See proposal.md, Why. `http-requests` (from `bootstrap-core-engine`) already owns a single request pipeline with a header-building step and per-phase timings, and `request-history` and `test-reports` already persist exchanges. Trace propagation is a thin cross-cutting layer: generate or parse ids before send, record them after, and optionally emit a span. The constraint from `config.yaml` is local-first, so nothing may leave the machine unless the user explicitly configures it.

## Goals / Non-Goals

**Goals:**
- Zero effect when disabled: no headers, no export, no extra dependencies doing work at runtime.
- Interoperate with any OpenTelemetry-compatible backend through standard headers and links.
- Deterministic, testable id handling.

**Non-Goals:**
- A trace viewer inside Rustman. The backend's UI does that better, so we link to it.
- Exporting metrics or logs.

## Decisions

### D1. Hand-rolled context handling, `opentelemetry` crates only for export (rejected: the full OTel SDK as the propagation path)
- **Propagation:** generating and parsing `traceparent`/B3/`baggage` is about 200 lines, with spec test vectors, and no global state.
- **Why not the SDK:** using it for propagation would need a global tracer provider and context propagation through our async tasks, which couples the core to OTel's runtime even when tracing is off.
- **Export:** only `opentelemetry-otlp` + `opentelemetry_sdk` behind the cargo feature `otlp`, initialized lazily the first time export is enabled.

### D2. Injection point
- **Where:** a pipeline stage in `rustman-http` runs after variable resolution and user headers, so user-supplied headers can be detected and win.
- **Other kinds:** the GraphQL, JSON-RPC and gRPC crates call the same stage through a small `TraceContextInjector` trait (HTTP headers or gRPC metadata).
- **Run scope:** the runner or exploration context holds the shared trace id for `scope: run`.

### D3. Randomness
Ids come from `rand`'s thread-local CSPRNG, and are regenerated in the astronomically unlikely all-zeros case. Tests inject a seeded generator so expected headers are reproducible.

### D4. Link templates
Simple `{placeholder}` substitution, validated when the workspace loads, with preset expansions documented in `docs/tracing.md`:

| Preset | Template |
|---|---|
| jaeger | `{base}/trace/{trace_id}` |
| tempo | Grafana Explore URL with a Tempo query for `{trace_id}` |
| honeycomb | `{base}/trace?trace_id={trace_id}` |
| datadog | `{base}/apm/trace/{trace_id}` |

Datadog uses 64-bit ids in some setups. The datadog preset also exposes `{trace_id_dd}` (the lower 64 bits as decimal).

### D5. Server-Timing parsing
A lenient parser that splits metrics on commas, keeps the first `dur` and `desc`, and ignores malformed entries with a debug log. Values attach to the exchange as `server_timings: [{name, dur_ms, desc}]`, rendered under the TTFB phase.

### D6. Export pipeline
`BatchSpanProcessor` with a queue of 2048, a 5-second scheduled delay, and drop-oldest when full. Spans are built after the exchange completes, using recorded timestamps, so export never sits on the request's critical path. URL query values pass through the `environments` redaction step. Headers are never exported (that also avoids leaking `Authorization`).

## Risks / Trade-offs

- [Injecting headers changes request semantics: strict CORS preflights, signed requests] → Off by default and per environment. Signed-request auth helpers compute signatures after injection, so the headers are covered. Docs note CORS implications for browser-facing tests.
- [Backends that ignore incoming context] → `traceresponse` handling records the backend's own id. Otherwise the link may miss, which is documented.
- [Accidental export to a shared collector] → Needs an explicit endpoint plus `enabled = true`. The environment variables are ignored unless `use_env = true`. The TUI status bar shows `OTLP →<host>` while exporting.

## Migration Plan

Additive. The `trace` fields are optional in history and reports (`rustman.report/v1` stays at v1), and older files without them remain valid.

## Open Questions

None. Additional link presets (New Relic, Lightstep) can be added later without spec changes, since templates are user-configurable.
