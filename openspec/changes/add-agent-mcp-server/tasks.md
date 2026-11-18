# Tasks

## 1. Server skeleton and transports

- [x] 1.1 Add the server side of `crates/rustman-mcp` with `rmcp` (`server`, `transport-io`, `transport-streamable-http-server`) and the `rustman mcp serve` subcommand with workspace discovery. Verify with an integration test: spawn the binary, `initialize` over stdio, and assert the server name, version and capabilities. Without a workspace, exit is 2.
- [x] 1.2 Route all logging to stderr in serve mode. Verify a test asserts stdout contains only valid JSON-RPC frames during a full session.
- [x] 1.3 Add the HTTP mode: axum router at `/mcp`, default `127.0.0.1:8765`, non-loopback refused without `--allow-remote`. Verify with CLI tests for both bind cases.
- [x] 1.4 Add bearer token middleware (`--token-file` or a generated token printed to stderr, constant-time compare) and Origin validation with `[agent].allowed_origins`. Verify with HTTP tests: no token gives 401, a wrong origin gives 403, and the valid case reaches `tools/list`.
- [x] 1.5 Add the `[agent]` table to the `rustman.toml` schema with defaults. Verify with config parsing tests, and document it in `docs/agent.md`.

## 2. Guardrails

- [x] 2.1 Build `AgentContext` per session: session-scoped environment and a `GovernedSender` from `[agent]` (allowlist fallback to environment hosts, read-only default, destructive gating, no CLI escalation). Verify with unit tests per permission combination.
- [x] 2.2 Add elicitation-based confirmation for mutating calls, with fail-closed `confirmation_unavailable` and `declined`. Verify with tests using an rmcp test client with and without the elicitation capability.
- [x] 2.3 Add the second redaction pass over serialized results (secret values plus base64 and URL-encoded forms, and sensitive headers) for tools, resources, logs and progress. Verify with a test against an echo server that reflects Authorization into the body.
- [x] 2.4 Add output limits: a 256 KiB result cap, 32 KiB body truncation with a `body_ref` LRU resource, and cursor pagination (default 50). Verify with tests: a 2 MiB body is truncated with the correct size, and 120 requests give pages of 50, 50 and 20.
- [x] 2.5 Add the append-only daily NDJSON audit writer, fail-closed. Verify with tests that each outcome type writes a line, and that a write failure (read-only dir) makes the tool call error.

## 3. Tool, resource and prompt catalog

- [x] 3.1 Add the workspace tools `list_requests`, `get_request`, `list_environments` and `select_environment`, with output schemas. Verify with tests that validate each `structuredContent` against its declared schema and that `select_environment` doesn't modify disk.
- [x] 3.2 Add the execution tools `send_request` (saved and ad-hoc) and `run_collection`. Verify with tests against a local server, including a blocked host returning `code: "blocked"`.
- [x] 3.3 Add the OpenAPI and exploration tools `import_openapi` (dry_run), `explore_plan`, `explore_run`, `explore_status`, `explore_cancel`, `get_report` and `list_findings`. Verify that a test with `explore_plan` sends zero requests to the fixture server, and that `list_findings` filters by severity.
- [x] 3.4 Add `search_history` with filters and pagination. Verify with a test seeded with mixed exchanges.
- [ ] 3.5 Add conditional registration of `graphql_introspect`/`graphql_query` and `list_webhook_captures` behind cargo features that mirror those capabilities. Verify that `tools/list` snapshot tests match per feature set.
  - TODO: `rustman-graphql` and `rustman-webhook` landed on main after this branch's base (6e25c34), so they are not in this tree; wire them after rebasing onto main. `tools::protocol_tools()` is the registration point and registers nothing; `catalog.rs` asserts the three tools are absent from `tools/list`. Add the `graphql-requests` and `webhook-inspector` features and the tools once those crates land. Rebasing needs commits, which this stream's working tree may not make, so the rebase and 3.5 are the next step on this branch. On main, the runner skips graphql requests (`UnsupportedKind`) unless `RunOptions.graphql` holds an executor, and the agent sets none, so `run_collection` cannot bypass the agent allowlist after the rebase; when 3.5 wires an executor for agents, it must send through `RunOptions.gate` (or the session's `GovernedSender`) and carry the confirmations.
- [x] 3.6 Add the resources `rustman://requests/`, `rustman://reports/`, `rustman://schemas/` and `rustman://bodies/`, with traversal protection. Verify with a test that `../` paths error.
- [x] 3.7 Add the prompts `explore-api` and `triage-findings`. Verify with a test that `prompts/get` output mentions `explore_plan` before `explore_run`.
- [x] 3.8 Add a structured error code catalog (`not_found`, `invalid_input`, `blocked`, `declined`, `confirmation_unavailable`, `busy`, `internal`), documented in `docs/agent.md`. Verify with a test that every tool error path uses a cataloged code.

## 4. Background jobs

- [x] 4.1 Add the job registry with `CancellationToken`, `max_jobs` (default 2, `busy` beyond), and a job id returned in under 2 s. Verify with tests: a third job is refused, and time to first response is under 2 s with a slow fixture.
- [x] 4.2 Add progress notifications at least every 5 s when a progress token is present. MCP cancellation and `explore_cancel` stop the job using the exploration interrupt semantics. Verify with tests: progress cadence, a cancelled job's status is `cancelled`, and `rustman explore resume` completes it from the CLI.

## 5. Integration and docs

- [x] 5.1 Run an end-to-end agent scenario test with the rmcp client: list requests, `explore_plan`, then `explore_run` (read-only), poll status, `list_findings`, then `get_report`, against the `bad-api` fixture from `add-openapi-explorer`. Verify on CI on Linux, macOS and Windows.
  - Note: `crates/rustman-mcp/tests/agent_e2e.rs` passed in CI on Linux, macOS and Windows (#63).
- [x] 5.2 Write `docs/agent.md`: setup for Claude Code/Desktop and generic clients, the permission model, HTTP mode security, the audit log, and the tool reference generated from schemas. Verify with a test that the tool reference section matches `tools/list`.
- [x] 5.3 Run `openspec validate add-agent-mcp-server --strict`. Verify it exits 0.

## Workflow follow-up

- Archive with `openspec archive add-agent-mcp-server` after `add-openapi-explorer` is archived.
