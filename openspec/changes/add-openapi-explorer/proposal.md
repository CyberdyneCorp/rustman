# Proposal

## Why

Most backends that Rustman users test already publish an OpenAPI document. Hand-writing a request per operation, then hand-writing negative, auth and lifecycle tests, is the slow part of API testing and the part people skip. Rustman can do it automatically: given an OpenAPI document, generate the collection, investigate the backend, and report every place where it disagrees with its contract or misbehaves on security, without anyone touching production by accident.

## What Changes

- **OpenAPI import** (`rustman import openapi <file|url>`) covers OpenAPI 3.0.x and 3.1.x in JSON or YAML. Swagger 2.0 is rejected with an actionable error. Local `$ref`s are resolved, and remote `$ref`s are off by default. The import generates one request file per operation, grouped into collections by tag. Example bodies come from spec examples or are synthesized from schemas. `servers` become environments, and `securitySchemes` become auth config with secret placeholders.
- **Re-import / sync** (`rustman import openapi --sync`) uses a three-way merge that preserves user edits and reports added, removed and changed operations.
- **Exploration** (`rustman explore plan|run|resume|status`) runs a four-phase campaign: discover → plan → execute → report.
  - *Discover* builds a resource dependency graph from OpenAPI `links` plus producer/consumer heuristics.
  - *Plan* expands a catalog of probe families: conformance, negative input, auth enforcement, optional two-identity BOLA/IDOR, undocumented status, method-not-allowed, error leakage, security headers, CRUD lifecycle chains and seeded property-based fuzzing.
  - *Execute* runs under a strict **safety policy**: read-only by default, `--allow-mutations` for POST/PUT/PATCH, `--allow-destructive` for DELETE, target-host allowlist, 10 req/s and concurrency 4, probe and time budgets, a production guard, and best-effort cleanup.
  - *Report* produces coverage (operations × documented status codes), findings with severity, evidence and reproduction (curl plus an exportable request file).
  - Explorations can be interrupted and resumed.
- **Test reports** define one canonical, versioned JSON report schema for runs and explorations, with renderers for JUnit XML, Markdown, self-contained HTML and SARIF (security findings), plus `rustman report diff`. Every renderer upholds the redaction invariant.

## Capabilities

### New Capabilities

- `openapi-import`: parsing OpenAPI documents and generating and syncing workspace collections, environments and auth config from them.
- `api-exploration`: the exploration engine, covering discovery, probe planning, the safety policy, execution, coverage, findings, reproduction and resume.
- `test-reports`: the canonical report schema, its renderers (JSON, JUnit, Markdown, HTML, SARIF), report diffing and redaction in reports.

### Modified Capabilities

None. `cli-runner` and `request-history` (from `bootstrap-core-engine`) are reused unchanged. The JSON and JUnit run reporters from `bootstrap-core-engine` must emit the canonical schema defined here. That's a constraint on their output, not a change to their requirements.

## Out of Scope

- GraphQL schema-driven exploration (a later change on top of `add-graphql-client`).
- Load and performance testing. Rate limits here exist for safety, not for throughput.
- Exploiting findings. Probes detect and stop: there's no payload chaining, no credential brute force and no DoS-style probes.
- AsyncAPI, RAML, API Blueprint, Postman collection import.
- Automatic remediation or patch suggestions for the backend.
- The agent-facing tools for exploration (they're in `add-agent-mcp-server`) and graphical dashboards (in `add-raven-visualizer`).

## Impact

- New crates: `crates/rustman-explore` (OpenAPI model, discovery, planner, executor, findings) and the full `crates/rustman-report` (renderers, diff, SARIF).
- New CLI subcommands in `rustman-cli`: `import openapi`, `explore`, and `report render|diff`.
- New workspace content: `[safety]` and `[explore]` tables in `rustman.toml`; `.rustman/explorations/<id>/` (state, journal, report); a `.rustman/openapi/` snapshot used for sync.
- New dependencies, with rationale in design.md: an OpenAPI 3.x model crate, `jsonschema` (already in core), `proptest` (seeded generation at runtime), `governor` (rate limiting), `serde-sarif`, and `minijinja` (HTML/Markdown templates).
- It defines the safety policy that `add-agent-mcp-server` enforces for every agent-triggered request.
