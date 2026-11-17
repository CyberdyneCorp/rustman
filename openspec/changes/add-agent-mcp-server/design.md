# Design

## Context

The engine (`bootstrap-core-engine`) and the exploration stack (`add-openapi-explorer`) already exist as libraries. `add-openapi-explorer` introduced `SafetyPolicy` and `GovernedSender` in `rustman-core`, the only path automated senders may use. `add-mcp-client` puts `rmcp` in `crates/rustman-mcp` for the client side, and this change adds the server side to the same crate. See proposal.md for the motivation.

## Goals / Non-Goals

**Goals:**
- An agent can go from nothing to a triaged exploration report using only MCP calls, with zero risk of leaking secrets or mutating data unless a human opted in twice (config plus confirmation).
- The tool catalog is a thin adapter: each tool maps to an existing engine call, with no business logic in the MCP layer.
- It works with the mainstream MCP clients (Claude Code, Claude Desktop, IDE agents) over stdio with no configuration beyond the command line.

**Non-Goals:**
- A multi-tenant or network-exposed server. HTTP mode is for local tools that can't spawn processes.
- Feature parity with the TUI (layout, history browsing UX).

## Decisions

### D1. `rmcp` (official Rust SDK) over a hand-rolled JSON-RPC server
`rmcp` provides the stdio and Streamable HTTP transports, `#[tool]` macros that derive input schemas via `schemars`, structured output, progress notifications, cancellation and elicitation. Hand-rolling these would repeat the Ghostman mistake of re-implementing a protocol. We pin a minor version and track the spec revision it implements in `docs/agent.md`.
*Alternative:* `mcp-server` community crates. Rejected because they're less maintained and lag the spec.

### D2. HTTP transport behind an axum wrapper that does auth before MCP
The Streamable HTTP service is mounted at `/mcp` in an `axum` router whose middleware checks the bearer token (constant-time compare) and Origin before the request reaches `rmcp`. Loopback-only binding is checked at parse time of the address. That gives three independent locks: bind address, token and origin.
*Alternative:* MCP OAuth authorization. Rejected as out of scope (local-only), and it would add an authorization server dependency.

### D3. One `AgentContext` per MCP session
It holds the session's active environment (copied from the workspace at session start and changed only by `select_environment`), a `GovernedSender` built from the `[agent]` config, the job registry and the audit writer. Nothing a session does writes workspace files except history (same as the CLI) and the audit log.

### D4. Permissions come only from `rustman.toml`
`[agent]` keys: `allow_mutations`, `allow_destructive`, `require_confirmation`, `allowed_hosts`, `allowed_origins`, `max_output_bytes`, `max_jobs`. There are deliberately no CLI flags that raise permissions. An agent that can run shell commands could otherwise relaunch the server with `--allow-mutations`. Editing a tracked config file shows up in `git diff` and code review.

### D5. Confirmation via MCP elicitation
For mutating calls we send `elicitation/create` with a schema `{confirm: boolean}` and a message naming the method, URL and environment. If the client lacks elicitation, we fail closed (`confirmation_unavailable`), unless the human set `require_confirmation = false`, an explicit acceptance of unattended mutations.

### D6. Redaction applied twice
Engine exchanges come back already redacted (core invariant). The MCP layer runs a second pass over the serialized result with the session's known secret values and sensitive header names before sending (defense in depth for echoes inside bodies, as in the spec scenario). The second pass works on the final JSON string, so no code path can skip it.

### D7. Output limiting with resource handles
Bodies over 32 KiB are stored in an in-memory LRU (64 MiB, per session) and exposed as `rustman://bodies/<id>` resources. Lists use opaque cursors (base64 of offset plus a snapshot hash). This keeps agent context windows small without losing data.

### D8. Jobs on tokio tasks with a registry
`explore_run` and `run_collection` spawn a task, register it with a `CancellationToken`, and return `{job_id}` immediately. Progress goes to `notifications/progress` when the request carried a progress token. MCP `notifications/cancelled` for the originating request and `explore_cancel` both trigger the token. Exploration state is the same journaled state as the CLI, so `rustman explore resume` works from a terminal afterwards.

### D9. Audit log: daily NDJSON, append-only, fail-closed
`O_APPEND` writes with one `write` call per line. If a line can't be written, the tool call errors, so agent actions are never unaudited. Rotation is by date. Retention is left to the user (the files are small).

## Risks / Trade-offs

- [Prompt-injection in API responses steers the agent into harmful calls] → the safety policy and read-only default bound what any steered agent can do. Mutations need config plus human confirmation. The audit log records everything.
- [Clients without elicitation make mutations unusable] → explicit `require_confirmation = false` escape hatch, documented as unattended mode.
- [Redaction second pass misses transformed secrets (base64-encoded tokens)] → also redact the base64 and URL-encoded forms of each secret value. Documented limitation for other transforms.
- [rmcp API churn] → pinned version, with the adapter isolated in one module.
- [Large workspaces make `list_requests` slow] → reuse the core workspace index, and paginate.

## Migration Plan

Additive. `rustman init` adds `.rustman/audit/` to the generated `.gitignore`. Existing workspaces without `[agent]` get the read-only defaults. A README section shows `claude mcp add rustman -- rustman mcp serve`.

## Open Questions

None. `rmcp` is pinned to `~3.10` once in `[workspace.dependencies]` (see `add-mcp-client` D1), so both halves of `rustman-mcp` always match.
