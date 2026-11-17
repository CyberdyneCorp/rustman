# Tasks

## 1. Crate, definitions and trust

- [ ] 1.1 Create `crates/rustman-mcp` depending on pinned `rmcp` (client, child-process, streamable-http-client features); verify `cargo build -p rustman-mcp` in CI on all three OSes.
- [ ] 1.2 Load MCP server definitions from `rustman.toml` and `mcp/servers/*.yaml` with interpolation and the command/url exclusivity error; unit tests for both scenarios.
- [ ] 1.3 Implement the stdio trust store (`.rustman/trust.yaml`, definition hash excluding env values) and `--trust`; tests for first spawn, changed args, and env-value rotation not requiring re-approval.

## 2. Transports and lifecycle

- [ ] 2.1 Stdio transport with env merge, cwd default, stderr capture and 5 s graceful shutdown; integration test with a fixture server that writes to stderr and one that exits with code 1.
- [ ] 2.2 Streamable HTTP transport with `Mcp-Session-Id`; test against a local fake server asserting the header on follow-up requests.
- [ ] 2.3 Legacy HTTP+SSE fallback on 4xx≠401; test with a fake legacy server and the `legacy SSE` label.
- [ ] 2.4 Initialize/version negotiation, capabilities display data, disabled operations for undeclared capabilities; tests for older-version and unsupported-version scenarios.

## 3. Tools, resources, prompts, completion

- [ ] 3.1 Paginated `tools/list` (max 100 pages) and `list_changed` refresh; tests with a 3-page fake server.
- [ ] 3.2 JSON Schema → form model and client-side validation with JSON-pointer errors and recorded override; unit tests for enum, nested object, array, required, and the `/path: required property missing` message.
- [ ] 3.3 `tools/call` result model (content types, structuredContent, isError vs protocol error) and `outputSchema` validation; tests for both scenarios.
- [ ] 3.4 Resources list/templates/read/subscribe with template expansion; tests including `notifications/resources/updated`.
- [ ] 3.5 Prompts list/get and `completion/complete` with 250 ms debounce; tests against the reference `server-everything` in CI.

## 4. Notifications, progress, cancellation

- [ ] 4.1 Notifications stream and `logging/setLevel`; test that debug logs appear after setting `debug`.
- [ ] 4.2 Progress tokens, 60 s default timeout, per-server/per-request override, cancellation on timeout and user action; tests for both timeout and user-cancel messages.

## 5. Server-initiated requests

- [ ] 5.1 `ApprovalBroker` trait with TUI and headless implementations (no auto-approve implementation exists); unit test that headless sampling returns `User rejected sampling request` and `--sampling-response` returns the canned answer.
- [ ] 5.2 Elicitation with schema form and accept/decline/cancel; tests for accept with valid email and headless decline.
- [ ] 5.3 Roots capability with workspace default and `list_changed`; test the default root URI.

## 6. OAuth

- [ ] 6.1 RFC 9728/8414/7591 discovery, PKCE S256, loopback redirect, `resource` parameter, pasted-URL fallback; integration test with a local mock authorization server.
- [ ] 6.2 Keychain token storage, auto-refresh, redaction, `rustman mcp-client logout`; tests for refresh-without-browser and redacted history.

## 7. Request files, assertions, runs, history

- [ ] 7.1 `kind: mcp` request files (server, operation, params) and save-from-TUI; round-trip test load → save → load.
- [ ] 7.2 Register `is_error`, `content_text_contains`, `structured`, `output_schema`, `tool_exists` assertions and captures; runner tests including the `tool_exists` failure message.
- [ ] 7.3 Session pool per run (single initialize per server); test that three requests spawn the stdio process once.
- [ ] 7.4 Record MCP exchanges in `request-history` with notifications (max 200) and redaction; test with 5 progress notifications.

## 8. CLI and TUI

- [ ] 8.1 `rustman mcp-client tools|call|resources|prompts|logout` with workspace names, `stdio:` and URLs, `--json`, exit code 1 on errors; `assert_cmd` tests; document in `docs/mcp-client.md`.
- [ ] 8.2 TUI MCP inspector pane (servers, info, Tools/Resources/Prompts/Notifications/Stderr tabs, forms, approval dialogs, pending-approval badge) on `add-terminal-ui`; ratatui `TestBackend` snapshot tests including the badge scenario.
