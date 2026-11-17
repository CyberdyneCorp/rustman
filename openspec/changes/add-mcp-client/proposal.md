# Proposal

## Why

MCP servers are becoming a primary API surface (for agents), yet testing one today means ad-hoc scripts or a browser-based inspector. Rustman can treat an MCP server like any other API: connect, explore its tools/resources/prompts, call them with schema-driven forms, save the calls as request files, assert on results, and replay them in CI — locally and with the same secret-handling guarantees as HTTP.

## What Changes

- `kind: mcp` request files become executable (reserved by `bootstrap-core-engine`): a server reference plus an operation (`tools/call`, `resources/read`, `prompts/get`, ...) and arguments.
- MCP server definitions in the workspace (`mcp_servers` in `rustman.toml` or `mcp/servers/*.yaml`): stdio (command, args, env, cwd) and Streamable HTTP (URL, headers, auth), with legacy HTTP+SSE fallback.
- Session lifecycle: initialize, protocol-version negotiation, server info and capabilities display, ping, clean shutdown.
- Tools: list (with pagination), call with JSON-Schema-driven argument forms, client-side input validation, `structuredContent` validation against `outputSchema`, `isError` handling.
- Resources (list, templates, read, subscribe/updated), prompts (list, get), argument completion.
- Logging level control and a notifications stream; progress and cancellation; per-call timeout (default 60 s).
- Server-initiated requests: sampling and elicitation are always shown to the human for approval (never auto-approved; headless denies by default); roots are exposed from a configured list.
- OAuth 2.1 for HTTP servers: protected-resource and authorization-server metadata discovery, dynamic client registration when offered, PKCE, token cache in the OS keychain.
- Assertions on tool results; history of MCP exchanges; CLI `rustman mcp-client tools|call|resources|prompts`; TUI MCP inspector pane.

## Capabilities

### New Capabilities

- `mcp-client`: connecting to remote MCP servers as a client over stdio, Streamable HTTP and legacy SSE; exercising tools, resources, prompts and completion; handling server-initiated requests with human approval; OAuth; and saving/replaying MCP calls as request files with assertions.

### Modified Capabilities

None (builds on `workspace`, `environments`, `assertions`, `cli-runner` and `request-history` from `bootstrap-core-engine` without changing their requirements).

## Out of Scope

- Rustman acting as an MCP server for agents — that is `agent-mcp-interface` (`add-agent-mcp-server`).
- Hosting or proxying MCP servers for third parties; MCP registry/marketplace browsing.
- Answering sampling requests with a built-in LLM: the human types or approves the response; LLM provider integration is a later change.
- Load testing MCP servers.

## Impact

- `crates/rustman-mcp` (client half; the server half arrives with `add-agent-mcp-server`), TUI pane in `crates/rustman-tui`, subcommands in `crates/rustman-cli`.
- New dependencies: `rmcp` (official Rust MCP SDK) with client, `transport-child-process` and `transport-streamable-http-client` features; `jsonschema` (already used by `assertions`) for argument/output validation; `oauth2` for the OAuth flow; `keyring` (already in core) for tokens.
- Spawning stdio servers executes user-configured commands — covered by an explicit trust prompt (see spec).
