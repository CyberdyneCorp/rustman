# Proposal

## Why

AI coding agents increasingly do API work themselves: reproducing a bug against an endpoint, checking a contract after a change, exploring an unfamiliar backend. Today they shell out to `curl` with no workspace, no environments, no redaction and no guardrails. If Rustman exposes its engine as an MCP server, any MCP-capable agent can use the same collections, environments, explorations and reports a human uses. It does so through the same safety policy, so an agent can't leak secrets or hit production with a DELETE.

## What Changes

- **`rustman mcp serve`** runs Rustman as an MCP server built on the official `rmcp` SDK, over **stdio** (default) or **Streamable HTTP**. HTTP binds to loopback by default, requires a bearer token and validates Origin.
- **Tools** with JSON input and output schemas and structured results:
  - Workspace: `list_requests`, `get_request`, `list_environments`, `select_environment`.
  - Execution: `send_request` (saved or ad-hoc), `run_collection`.
  - OpenAPI: `import_openapi`, `explore_plan`, `explore_run`, `explore_status`, `explore_cancel`, `get_report`, `list_findings`.
  - History: `search_history`.
  - Conditional, registered only when the capability exists: `graphql_introspect` and `graphql_query` (`add-graphql-client`), and `list_webhook_captures` (`add-webhook-inspector`).
- **Resources** expose request files, reports and cached schemas. **Prompts** are `explore-api` and `triage-findings`.
- **Guardrails**:
  - Every agent-triggered request passes the safety policy from `add-openapi-explorer`.
  - Agent mode is read-only by default. Mutations require a `[agent]` opt-in in `rustman.toml`, plus human confirmation through MCP elicitation when the client supports it.
  - Secrets are redacted in every result.
  - Output is capped with truncation and pagination cursors.
  - An append-only audit log is kept in `.rustman/audit/`.
- **Background jobs**: long explorations and runs return a job id immediately, stream progress notifications and are cancellable.

## Capabilities

### New Capabilities

- `agent-mcp-interface`: Rustman acting as an MCP server for AI agents. Covers transports and their security, the tool/resource/prompt catalog, agent permissions and confirmation, redaction and output limits, auditing, and background jobs.

### Modified Capabilities

None. It reuses `workspace`, `environments`, `http-requests`, `cli-runner` and `request-history` (from `bootstrap-core-engine`) and `api-exploration` / `test-reports` (from `add-openapi-explorer`) without changing their requirements.

## Out of Scope

- Rustman acting as an MCP *client* (`add-mcp-client`).
- Remote or multi-user hosting of the agent interface, OAuth-protected MCP authorization and TLS termination. HTTP mode is a local-machine convenience.
- Editing or creating request files from agent tools. Agents get read access to the workspace. Write tools may come in a later change.
- Agent access to secret values in any form, permanently. Agents can reference secrets by name, never read them.
- MCP sampling (asking the agent's LLM for completions).

## Impact

- New module in `crates/rustman-mcp` (server side) and the `mcp serve` subcommand in `rustman-cli`. The `rmcp` dependency with the `server`, `transport-io` and `transport-streamable-http-server` features.
- New `[agent]` table in `rustman.toml`. New `.rustman/audit/` directory (git-ignored by `rustman init`'s `.gitignore`).
- Depends on `add-openapi-explorer` for the safety policy (`GovernedSender`) and the exploration and report tools. That change must land first.
