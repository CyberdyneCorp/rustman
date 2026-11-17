# Design

## Context

`bootstrap-core-engine` reserves `kind: mcp`, provides variable resolution, secrets/keychain, the assertion registry, the runner and history. `add-terminal-ui` provides panes, forms and dialogs. `add-agent-mcp-server` will later add the *server* half to the same `rustman-mcp` crate, so client and server must share protocol types.

## Goals / Non-Goals

**Goals:**
- Full coverage of the client side of the current MCP spec (lifecycle, tools, resources, prompts, completion, logging, progress, cancellation, sampling, elicitation, roots, OAuth).
- Deterministic, replayable MCP calls in CI via request files.
- Human-in-the-loop for every server-initiated request that could leak data or act on the user's behalf.

**Non-Goals:**
- An embedded LLM to answer sampling.
- Supporting pre-2024-11-05 draft protocol versions.

## Decisions

### D1. SDK: official `rmcp` (rejected: hand-rolled JSON-RPC, `mcp-rust-sdk` community crates)
`rmcp` is the official Rust SDK, tracks spec revisions, provides child-process and Streamable HTTP client transports, typed models for every message, and a `ClientHandler` trait for server-initiated requests. Shared models also serve the server half in `add-agent-mcp-server`. Hand-rolling would duplicate version-negotiation and transport edge cases (session IDs, resumable SSE). Pin a minor version; wrap it behind our `McpSession` facade so SDK churn stays inside `rustman-mcp`.

### D2. Legacy HTTP+SSE fallback
If `rmcp` lacks a legacy SSE client transport in the pinned version, implement a thin `Transport` adapter (GET SSE → `endpoint` event → POST messages) using `eventsource-stream` over `rustman-http`. Fallback triggers only on 4xx≠401 from the initialize POST, per the spec's backwards-compatibility guidance.

### D3. Server-initiated requests via an approval broker
`ClientHandler` callbacks for sampling/elicitation forward to an `ApprovalBroker` trait with two implementations: TUI (enqueues a dialog, awaits the user's answer through a oneshot channel) and headless (deny/decline, or answer from `--sampling-response` file). There is no "auto-approve" implementation by construction — security property enforced by type, not config.

### D4. JSON Schema → form
Forms are generated from a normalized subset of JSON Schema 2020-12 (type, enum, const, properties, required, items, default, description, format, oneOf/anyOf rendered as a variant picker). Unsupported keywords fall back to the raw JSON editor for that subtree. Validation uses the `jsonschema` crate already used by `assertions`, so form validation and assertion validation agree.

### D5. OAuth via `oauth2` crate + loopback redirect
Discovery follows RFC 9728 (protected resource metadata) → RFC 8414 (AS metadata) → RFC 7591 (dynamic registration). Loopback redirect on `127.0.0.1:<ephemeral>`; browser opened with `open`/`webbrowser`. Tokens stored via `keyring` under `rustman/mcp/<server-hash>`. Reuses the token-cache abstraction introduced for HTTP OAuth in `bootstrap-core-engine`.

### D6. Stdio trust store
Approval of a stdio command is keyed by SHA-256 of `(command, args, sorted env keys, cwd)`; env *values* are excluded so rotating a secret doesn't require re-approval, but adding a new env var does. Stored in `.rustman/trust.yaml` (git-ignored, per machine) — trust is never shared through Git.

### D7. Session pool in runs
The runner holds a `HashMap<server_name, McpSession>` for the run's lifetime; sessions are shut down in `Drop`/explicit end with the 5 s graceful stdio shutdown.

## Risks / Trade-offs

- [MCP spec revisions move fast] → negotiation supports the latest plus previous revisions; contract tests run against the official `@modelcontextprotocol/server-everything` reference server in CI (Node available in CI only for tests).
- [Stdio servers are arbitrary code execution] → explicit trust prompt, `--trust` required headless, full command shown.
- [Sampling content can contain secrets from the workspace] → shown verbatim to the human only; never logged unredacted.
- [OAuth browser flow unavailable over SSH] → print the URL and accept a pasted redirect URL as fallback.

## Migration Plan

Additive; `kind: mcp` request files become executable. No data migration.

## Open Questions

- Should approved sampling responses be cacheable for replay in CI (keyed by request hash)? Deferrable; `--sampling-response` covers the CI case for now.
