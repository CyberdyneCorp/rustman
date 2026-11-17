# Tasks

## 1. Crate and request model

- [ ] 1.1 Create `crates/rustman-jsonrpc` depending on `rustman-core` and `rustman-http`, and register `kind: jsonrpc` in the request-kind registry. Verify: `cargo build --workspace`; loading a `kind: jsonrpc` fixture no longer reports an unknown kind.
- [ ] 1.2 Implement the `jsonrpc` file model (endpoint, method, params list or map, id, notification, batch, subscription, settings.retry, idempotent) with load-time errors for missing method, scalar params, more than 100 batch calls, status assertions over WebSocket, result assertions on notifications, and subscriptions over HTTP. Verify: one fixture test per error message in the spec.
- [ ] 1.3 Document the kind in `docs/request-files.md` with Ethereum, Bitcoin Core and Solana examples. Verify: a doc test loads every example.

## 2. Envelope and HTTP transport

- [ ] 2.1 Implement envelope serialization (sequential ids per connection, explicit ids, notifications) and response classification (result, error, malformed, id mismatch). Verify: unit tests for each spec scenario in "Request id strategy", "Notifications" and "Success and error responses".
- [ ] 2.2 Send single calls over `rustman-http`, reusing timeouts, TLS, proxies and auth, with HTTP error mapping (non-2xx non-RPC body → `HTTP <status>`, exit class 3). Verify: integration tests against a local hyper stub for success, error object, 502 HTML and timeout.
- [ ] 2.3 Implement batches with id-based matching, missing-item failures and whole-batch rejection. Verify: tests for out-of-order, missing item and server-rejects-batch.
- [ ] 2.4 Implement retry for `idempotent: true` calls (429/502/503/504, `-32005`, connection errors, timeouts; exponential backoff; `Retry-After`), recording each attempt. Verify: stub test showing two attempts in history; a test showing non-idempotent calls aren't retried.

## 3. WebSocket and subscriptions

- [ ] 3.1 Add the WebSocket transport (`tokio-tungstenite` + rustls via the `rustman-http` TLS config) with id matching that ignores unrelated frames, plus a per-run connection cache. Verify: integration test with a WebSocket stub that interleaves a notification before the response.
- [ ] 3.2 Implement the subscription triple with `notification_id_path`, headless bounds (`max_events` 10, `max_duration_ms` 30000), unsubscribe at the end, and `$.events` for assertions. Verify: stub test for the `newHeads` scenario (3 events, unsubscribe sent); a duration-bound test.

## 4. Assertions, captures, history

- [ ] 4.1 Add the `rpc_error` assertion (`code`, `message_contains`, `none`) to the assertion engine. Verify: unit tests for expected error, unexpected success and wrong code messages.
- [ ] 4.2 Evaluate JSONPath assertions and captures against the full envelope, per item for batches. Verify: tests for capturing `$.result` and asserting `$.error.code`.
- [ ] 4.3 Record calls, batches and subscriptions as exchanges (events capped at 1000) with redaction of params and headers. Verify: history test showing `«redacted:api_key»` inside params.

## 5. OpenRPC catalogs

- [ ] 5.1 Implement `rustman rpc discover` (cache in `.rustman/cache/openrpc/`) and `[jsonrpc.catalogs]` file links. Verify: stub test for discover success and the `-32601` message with exit 1.
- [ ] 5.2 Validate method and params against the catalog with "did you mean" suggestions, plus `--no-validate`. Verify: tests for the unknown-method (exit 2) and wrong-type scenarios.

## 6. CLI

- [ ] 6.1 Implement `rustman rpc call` (raw, json, notify, subscribe, header, id flags) and `rustman rpc batch` (JSON array, YAML, JSON lines, stdin, chunks of 100). Verify: `assert_cmd` tests for each scenario, including exit codes 0, 1, 2 and 3.
- [ ] 6.2 Add `rpc` to shell completions and `--help` examples. Verify: completion snapshot test.

## 7. TUI pane and agent tool

- [ ] 7.1 Register the `jsonrpc` pane (Call, Batch, Headers, Auth, Assertions; viewer Result, Error, Events, Timing) with catalog completion and live subscriptions stopped by `Esc`. Verify: ratatui test-backend snapshots for completion and a live event list; `Esc` sends unsubscribe in a stub test.
- [ ] 7.2 Register the conditional `rpc_call` agent tool with safety policy checks, `agent_read_only_methods` globs and `@preset` expansion, and refusal of subscriptions. Verify: MCP tool tests for the allowlist refusal, an unknown preset (exit 2) and the non-read-only refusal.
- [ ] 7.3 Add the method-guard registry, evaluated before every send from every face and before the agent allowlist. Verify: a test guard refuses a method from CLI, TUI action and agent tool alike, with exit code 4.

## 8. Integration

- [ ] 8.1 End-to-end: `rustman run` over a fixture collection against a local Anvil node (when present in CI) and a stub server, covering a call, a batch, a subscription and an error assertion. Verify: the CI job passes, and the JSON report contains `jsonrpc` exchanges in the `rustman.report/v1` format.
