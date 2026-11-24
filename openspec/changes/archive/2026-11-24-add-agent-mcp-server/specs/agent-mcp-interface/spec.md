# Spec Delta

## Purpose

Lets AI agents drive Rustman over the Model Context Protocol. Agents can browse the workspace, send requests, run collections, import OpenAPI and run explorations through the same engine and safety policy a human uses, with secrets redacted and every action audited.

## ADDED Requirements

### Requirement: Serve command and stdio transport
`rustman mcp serve` SHALL start an MCP server for the workspace found from the current directory (or `--workspace <dir>`) using the stdio transport by default. Stdout SHALL carry only MCP messages and logs SHALL go to stderr. Without a workspace, the server SHALL exit with code 2 before the handshake.

#### Scenario: Agent connects over stdio
- **GIVEN** an MCP client configured to launch `rustman mcp serve`
- **WHEN** it sends `initialize`
- **THEN** the server SHALL respond with server name `rustman`, its version and the tools, resources, prompts and logging capabilities

#### Scenario: No workspace
- **GIVEN** a directory with no `rustman.toml` in it or its ancestors
- **WHEN** `rustman mcp serve` is started
- **THEN** it SHALL exit with code 2 and a message on stderr

### Requirement: Streamable HTTP transport binding
`rustman mcp serve --http [<addr>]` SHALL serve the MCP Streamable HTTP transport at path `/mcp`, binding to `127.0.0.1:8765` by default. A non-loopback address SHALL be refused unless `--allow-remote` is also given, and then a warning SHALL be printed.

#### Scenario: Non-loopback refused
- **WHEN** the user runs `rustman mcp serve --http 0.0.0.0:8765`
- **THEN** the command SHALL exit with code 2 explaining that `--allow-remote` is required

### Requirement: HTTP bearer token
In HTTP mode, every request SHALL carry `Authorization: Bearer <token>`. The token SHALL come from `--token-file`, or else be generated at startup (≥ 32 random bytes, base64url) and printed once to stderr. Missing or wrong tokens SHALL get HTTP 401 and SHALL NOT reach the MCP layer.

#### Scenario: Missing token
- **GIVEN** the server is running in HTTP mode
- **WHEN** a client POSTs to `/mcp` without an Authorization header
- **THEN** the response SHALL be 401 and no tool SHALL execute

### Requirement: HTTP Origin validation
In HTTP mode, requests that carry an `Origin` header SHALL be rejected with HTTP 403 unless the origin is a loopback origin or listed in `[agent].allowed_origins`, to prevent DNS-rebinding attacks from browsers.

#### Scenario: Foreign origin
- **WHEN** a request arrives with `Origin: https://evil.example`
- **THEN** the response SHALL be 403

### Requirement: Structured tools with schemas
Every tool SHALL declare an input JSON Schema and an output JSON Schema and SHALL return `structuredContent` that conforms to its output schema, plus a short text summary in `content`. Tool errors caused by bad input or policy SHALL be returned as tool results with `isError: true` and a machine-readable `code`, not as protocol errors.

#### Scenario: Output conforms to schema
- **WHEN** an agent calls `list_requests`
- **THEN** the result's `structuredContent` SHALL validate against the tool's declared output schema

#### Scenario: Invalid input
- **WHEN** an agent calls `get_request` with a path that does not exist
- **THEN** the result SHALL have `isError: true` and `code: "not_found"`

### Requirement: Workspace tools
The server SHALL provide `list_requests` (filter by collection, kind, text), `get_request` (one request file with secret values as `{{name}}` references), `list_environments` (names, active flag, variable names) and `select_environment` (changes the active environment for this server session only, without writing to disk).

#### Scenario: Session-scoped environment
- **GIVEN** the workspace's active environment is `local`
- **WHEN** an agent calls `select_environment` with `staging`
- **THEN** subsequent tool calls in that session SHALL use `staging`
- **AND** the workspace's on-disk active environment SHALL remain `local`

### Requirement: Request execution tools
`send_request` SHALL execute either a saved request (`path`, optional `vars`) or an ad-hoc request (`method`, `url`, `headers`, `body`) and return the redacted exchange: status, headers, body (subject to output limits), timings and assertion results. `run_collection` SHALL run a path like `rustman run` and return the run report summary.

#### Scenario: Send saved request
- **GIVEN** a saved request `users/get-user.rman.yaml`
- **WHEN** an agent calls `send_request` with that path and `vars: {"id": "42"}`
- **THEN** the result SHALL include the response status, timings and the redacted response headers and body

### Requirement: OpenAPI and exploration tools
The server SHALL provide `import_openapi` (supports `dry_run`), `explore_plan` (never sends traffic), `explore_run`, `explore_status`, `explore_cancel`, `get_report` (summary or a requested format) and `list_findings` (filter by severity and operation), with the semantics of `openapi-import`, `api-exploration` and `test-reports`.

#### Scenario: Plan before run
- **WHEN** an agent calls `explore_plan` for a spec and target
- **THEN** the result SHALL list probes with their allowed or blocked status
- **AND** the target SHALL receive no requests

### Requirement: History tool
`search_history` SHALL search recorded exchanges by URL substring, method, status range and time range, returning redacted exchange summaries newest first, with pagination.

#### Scenario: Recent failures
- **WHEN** an agent calls `search_history` with `status: "500-599"` and `limit: 10`
- **THEN** at most 10 redacted exchanges with 5xx statuses SHALL be returned, newest first

### Requirement: Conditional protocol tools
`graphql_introspect` and `graphql_query` SHALL be registered only when the build includes the `graphql-requests` capability, and `list_webhook_captures` only when it includes `webhook-inspector`. Absent tools SHALL NOT appear in `tools/list`.

#### Scenario: Build without webhook inspector
- **GIVEN** a build without the `webhook-inspector` capability
- **WHEN** an agent calls `tools/list`
- **THEN** `list_webhook_captures` SHALL NOT be listed

### Requirement: Resources
The server SHALL expose request files as `rustman://requests/<path>`, reports as `rustman://reports/<run-or-exploration-id>` and cached schemas as `rustman://schemas/<name>`, with `resources/list` and `resources/read`. Contents SHALL be redacted. Paths escaping the workspace SHALL be refused.

#### Scenario: Path traversal refused
- **WHEN** an agent reads `rustman://requests/../../etc/passwd`
- **THEN** the server SHALL return an error and SHALL NOT read outside the workspace

### Requirement: Prompts
The server SHALL offer the prompts `explore-api` (arguments: `spec`, `target`) and `triage-findings` (argument: `exploration_id`). These guide an agent through plan → review → run → report, and through grouping findings by root cause with reproduction steps.

#### Scenario: Get explore prompt
- **WHEN** an agent calls `prompts/get` for `explore-api` with a spec and a target
- **THEN** the returned messages SHALL instruct calling `explore_plan` before `explore_run`

### Requirement: Safety policy on every agent request
Every outbound request triggered through the agent interface SHALL pass the safety policy defined by `api-exploration`, including saved requests and `run_collection`. The allowlist SHALL be `[agent].allowed_hosts`, falling back to the hosts of the session's active environment. Blocked requests SHALL return `isError: true` with `code: "blocked"` and the reason.

#### Scenario: Unlisted host blocked
- **GIVEN** no allowlist entry for `example.org`
- **WHEN** an agent calls `send_request` with `url: "https://example.org/"`
- **THEN** no request SHALL be sent and the result SHALL have `code: "blocked"` with reason `host not allowed`

### Requirement: Read-only agent mode by default
Unless `[agent].allow_mutations = true` in `rustman.toml`, the agent interface SHALL allow only GET, HEAD and OPTIONS requests and SHALL force exploration probes to read-only. `[agent].allow_destructive = true` SHALL be required for DELETE and other methods outside GET, HEAD, OPTIONS, POST, PUT and PATCH, and SHALL imply `allow_mutations = true`, as `--allow-destructive` implies `--allow-mutations` for `rustman explore`. CLI flags SHALL NOT be able to raise agent permissions above the config.

#### Scenario: POST refused by default
- **GIVEN** `rustman.toml` has no `[agent]` table
- **WHEN** an agent calls `send_request` with method `POST`
- **THEN** the result SHALL have `code: "blocked"` with reason `mutations not allowed for agents`

#### Scenario: DELETE needs allow_destructive
- **GIVEN** `rustman.toml` has `[agent] allow_mutations = true` and no `allow_destructive`
- **WHEN** an agent calls `send_request` with method `DELETE`
- **THEN** the result SHALL have `code: "blocked"` with reason `destructive not allowed for agents`

### Requirement: Human confirmation of mutations
When mutations are allowed and the client advertises the elicitation capability, each mutating request and each mutating `explore_run` SHALL first ask the human to confirm, via elicitation that shows the method, URL and environment. A decline or cancel SHALL block the action. Without elicitation support, `[agent].require_confirmation` (default `true`) SHALL make such calls fail with `code: "confirmation_unavailable"`.

#### Scenario: Human declines
- **GIVEN** mutations are allowed and the client supports elicitation
- **WHEN** an agent calls `send_request` with `DELETE /users/7` and the human declines
- **THEN** no request SHALL be sent and the result SHALL have `code: "declined"`

#### Scenario: Client without elicitation
- **GIVEN** mutations are allowed, `require_confirmation` is default and the client lacks elicitation
- **WHEN** an agent calls `send_request` with method `PUT`
- **THEN** the result SHALL have `code: "confirmation_unavailable"`

### Requirement: Secret redaction in results
No tool result, resource content, prompt, log notification or progress notification SHALL contain a secret value, or the value of `Authorization`, `Proxy-Authorization`, `Cookie`, `Set-Cookie` or `X-Api-Key` headers. They SHALL appear as `«redacted:<name>»`. No tool SHALL return secret values, even on request.

#### Scenario: Token in response echo
- **GIVEN** a backend that echoes the request's Authorization header in its JSON body, and the token is secret `api_token`
- **WHEN** an agent calls `send_request`
- **THEN** the returned body SHALL contain `«redacted:api_token»` and not the token value

### Requirement: Output size limits and pagination
Each tool result SHALL be capped at 256 KiB of serialized output (`[agent].max_output_bytes`). Bodies over 32 KiB SHALL be truncated with `truncated: true`, the full size and a `body_ref` readable through `resources/read`. List tools SHALL default to 50 items and return a `next_cursor` when more exist.

#### Scenario: Large body truncated
- **GIVEN** a response body of 2 MiB
- **WHEN** it is returned from `send_request`
- **THEN** the body SHALL be truncated to 32 KiB with `truncated: true`, `size: 2097152` and a `body_ref`

#### Scenario: Paginated list
- **GIVEN** a workspace with 120 request files
- **WHEN** an agent calls `list_requests` twice, passing the first `next_cursor`
- **THEN** the calls SHALL return 50 and 50 distinct items, and a third call SHALL return the remaining 20 with no `next_cursor`

### Requirement: Audit log
Every tool call SHALL append one JSON line to `.rustman/audit/<YYYY-MM-DD>.ndjson` with timestamp, session id, client name, tool, redacted arguments, outcome (`ok`, `error`, `blocked`, `declined`) and, for outbound requests, method and URL. The server SHALL only append, never rewrite or delete. Failing to write the audit log SHALL fail the tool call.

#### Scenario: Blocked call audited
- **WHEN** an agent's `send_request` is blocked by the safety policy
- **THEN** a line with `tool: "send_request"`, `outcome: "blocked"` and the reason SHALL be appended to today's audit file

### Requirement: Background jobs with progress and cancellation
`explore_run` and `run_collection` SHALL return a `job_id` within 2 s while work continues in the background. When the client supplied a progress token, the server SHALL send progress notifications at least every 5 s. `explore_status` and the job status SHALL report progress, and `explore_cancel` (or MCP request cancellation) SHALL stop the job with the interrupt semantics of `api-exploration`.

#### Scenario: Cancel running exploration
- **GIVEN** an exploration job started via `explore_run`
- **WHEN** the agent calls `explore_cancel` with its `job_id`
- **THEN** dispatch SHALL stop, cleanup SHALL run per policy, and status SHALL become `cancelled`, resumable later

### Requirement: Concurrency limits
The server SHALL run at most 2 background jobs at once (`[agent].max_jobs`). Further job-starting calls SHALL fail with `code: "busy"`. Synchronous `send_request` calls SHALL share the safety policy's rate limit and concurrency across the whole session.

#### Scenario: Third job refused
- **GIVEN** two exploration jobs running
- **WHEN** an agent calls `explore_run` again
- **THEN** the result SHALL have `isError: true` and `code: "busy"`
