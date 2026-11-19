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
`rmcp` is the official Rust SDK, tracks spec revisions, provides child-process and Streamable HTTP client transports, typed models for every message, and a `ClientHandler` trait for server-initiated requests. Shared models also serve the server half in `add-agent-mcp-server`. Hand-rolling would duplicate version-negotiation and transport edge cases (session IDs, resumable SSE). Pin `rmcp = "~3.10"` (MSRV 1.88) in `[workspace.dependencies]`, shared with `add-agent-mcp-server`, and re-pin to the latest minor at each Rustman release; wrap it behind our `McpSession` facade so SDK churn stays inside `rustman-mcp`. Stdio sessions spawn the server with `tokio::process` over rmcp's `transport-async-rw` instead of the `transport-child-process` feature: that feature waits a fixed 3 s on shutdown (the spec says 5 s), hides the exit code, and pulls in `process-wrap`. Because servers often run behind a launcher (`npx`, `uvx`, `sh -c`), the child starts in a process group of its own on Unix and the whole group is killed (safe `killpg` via `rustix`); on Windows the tree is killed with `taskkill /T /F` while its root still runs, and a bare command name is looked up with `PATHEXT` so `npx` finds `npx.cmd`. `initialize` is bounded by the server's request timeout, stderr lines by 4 KiB each, and SSE events by 16 MiB (rmcp's default, which a custom `StreamableHttpClient` must enforce itself). The Streamable HTTP client is rmcp's transport worker over our own `StreamableHttpClient` implementation on `rustman-http` (the bundled one needs `reqwest`, which `cargo deny` rejects). Workspace trust lives in `rustman_core::trust` as a generic subject → fingerprint store so `add-plugin-system` can reuse it.

### D2. Legacy HTTP+SSE fallback
If `rmcp` lacks a legacy SSE client transport in the pinned version, implement a thin `Transport` adapter (GET SSE → `endpoint` event → POST messages) using `eventsource-stream` over `rustman-http`. Fallback triggers only on 4xx≠401 from the initialize POST, per the spec's backwards-compatibility guidance.

### D3. Server-initiated requests via an approval broker
`ClientHandler` callbacks for sampling/elicitation forward to an `ApprovalBroker` trait with two implementations: TUI (enqueues a dialog, awaits the user's answer through a oneshot channel) and headless (deny/decline, or answer from `--sampling-response` file). There is no "auto-approve" implementation by construction — security property enforced by type, not config. An elicitation can only be accepted with content that passed its `requestedSchema` (the accepted value is a type only the validation constructs).

### D3a. Notifications recorded by a transport tap
rmcp runs each notification handler in a task of its own, so a handler may run after the answer that followed its notification. The client wraps every transport in a tap that records notifications in the receive loop, in arrival order, before the answer is dispatched: the notifications stream is ordered and a call's exchange holds exactly the notifications sent before its answer. Timeouts and user cancellation select over rmcp's `RequestHandle` and send `notifications/cancelled` through it.

### D4. JSON Schema → form
Forms are generated from a normalized subset of JSON Schema 2020-12 (type, enum, const, properties, required, items, default, description, format, oneOf/anyOf rendered as a variant picker). Unsupported keywords fall back to the raw JSON editor for that subtree. Validation uses the `jsonschema` crate already used by `assertions`, so form validation and assertion validation agree.

### D5. OAuth over `rustman-http` + loopback redirect (rejected: the `oauth2` crate)
Discovery follows RFC 9728 (protected resource metadata) → RFC 8414 (AS metadata) → RFC 7591 (dynamic registration). Loopback redirect on `127.0.0.1:<ephemeral>`; browser opened with `open`/`webbrowser`, with a pasted redirect URL as fallback. Tokens stored in the `SecretStore` (OS keychain, else `.rustman/secrets.yaml`) under `mcp/<server-hash>`, with the token endpoint and client so a refresh needs no rediscovery. The flow reuses the PKCE and loopback pieces of the HTTP OAuth helper in `rustman-http` (now public) and sends its metadata, registration and token requests through the `HttpEngine`; the `oauth2` crate was rejected because its HTTP clients pull `reqwest` (denied by `cargo deny`) and it has no RFC 9728/7591 support, so it would add little over the existing helper.

### D6. Stdio trust store
Approval of a stdio command is keyed by SHA-256 of `(command, args, env sorted by key, cwd)`, where a secret substituted into any field counts only by its name: rotating a secret doesn't require re-approval, but adding an env var or changing a literal env value does (values such as `PATH`, `NODE_OPTIONS` or `LD_PRELOAD` decide what code runs). Stored in `.rustman/trust.yaml` (git-ignored, per machine) — trust is never shared through Git. Each entry is not the hash itself but HMAC-SHA256(machine key, workspace location ‖ subject ‖ hash): the key is 32 random bytes in `trust.key` in the user's data directory (`RUSTMAN_DATA_DIR` overrides it, for tests and sandboxes), created on the first approval with `0600` on Unix. An unkeyed hash is computable by anyone who can read the definition, so a repository could ship valid approvals (Git-based provenance checks do not help in a ZIP/tarball with no `.git`); with the key, a trust file written by a repository, copied from another machine or from another workspace matches nothing, whatever Git says. Binding the workspace location matters because a relative `command` names a different program in another folder; the cost is re-approval when a workspace folder moves. The Git and symlink provenance checks stay as a second guard. The store (`rustman_core::trust`) stays generic: callers pass a subject and their own fingerprint; the key is an implementation detail (`TrustStore::load`, or `load_with_key` for tests).

Stdio servers start from a minimal allowlist of the parent environment (`PATH`, `HOME`, `USER`, locale, temp folders, and the Windows system variables `node`/`npx`/`python` need) plus the definition's `env`, never the whole parent environment: tokens and cloud credentials in the user's shell are not leaked to a workspace-chosen command. There is no opt-in to inherit more; a definition passes what its server needs through `env` (with `{{secret}}` interpolation).

### D6a. Stopping servers on every exit path
Each stdio server runs in a process group of its own (Unix) so the whole tree can be killed; the same isolation means terminal signals never reach it. Live process trees are therefore listed in a process-wide registry (`process_tree`), removed when stopped or reaped. The CLI treats SIGINT, SIGTERM and SIGHUP alike (first: cancel, graceful shutdown with the 5 s grace; second: quit at once), and the forced exit (`std::process::exit`, which runs no destructor) calls `kill_all_servers()` first. Windows job objects (which would also cover a hard kill of Rustman itself) are a follow-up.

### D6b. Resource limits on server output
A stdio server's stdout goes through a line-length limit of 16 MiB (the SSE event limit): a longer line ends the stream with an error, so the session closes and the requests in flight fail with a clear message instead of the line being buffered (or silently dropped by the SDK and the request timing out).

### D7. Session pool in runs
The runner holds a `HashMap<server_name, McpSession>` for the run's lifetime; sessions are shut down in `Drop`/explicit end with the 5 s graceful stdio shutdown.

## Risks / Trade-offs

- [MCP spec revisions move fast] → negotiation supports the latest plus previous revisions; contract tests run against the official `@modelcontextprotocol/server-everything` reference server in CI (Node available in CI only for tests).
- [Stdio servers are arbitrary code execution] → explicit trust prompt, `--trust` required headless, full command and env shown; approvals keyed per machine; minimal inherited environment.
- [Sampling content can contain secrets from the workspace] → shown verbatim to the human only; never logged unredacted.
- [OAuth browser flow unavailable over SSH] → print the URL and accept a pasted redirect URL as fallback.

## Migration Plan

Additive; `kind: mcp` request files become executable. No data migration.

## Open Questions

- Should approved sampling responses be cacheable for replay in CI (keyed by request hash)? Deferrable; `--sampling-response` covers the CI case for now.
