# Tasks

## 1. Server skeleton

- [ ] 1.1 Create `crates/rustman-mock` with a hyper 1.x HTTP/1.1 + h2c server, `--host/--port`, port 0 and the start banner; verify an integration test for the banner text and the address-in-use exit code 2.
- [ ] 1.2 Loopback guard with `--allow-remote`; verify the CLI refusal message and exit code 2 for `--host 0.0.0.0`.
- [ ] 1.3 Request limits (10 MiB → 413, 100 headers / 64 KiB → 431) configurable under `[mock]`; verify raw-socket tests for each limit.
- [ ] 1.4 Graceful Ctrl+C shutdown (5 s drain, exit 130); verify a test with a delayed in-flight request.

## 2. Routing and responses

- [ ] 2.1 Load the document through `openapi-import` (refs policy, diagnostics, exit 2); verify the dangling-ref test prints the JSON pointer.
- [ ] 2.2 Segment-trie router with literal-over-parameter precedence, base-path stripping, 404 and 405 + `Allow`; verify the `/users/me` vs `/users/{id}` and `DELETE /users` tests.
- [ ] 2.3 problem+json error builder used by every generated error; verify a snapshot of the 404 body.
- [ ] 2.4 Response selection (override → Prefer → lowest 2xx/default), example precedence and content negotiation with 406; verify the `cat` default, `Prefer: code=404, example=notFound`, undocumented-code 400 and 406 tests.
- [ ] 2.5 Seeded synthesis via the `openapi-import` synthesizer; verify identical bodies for repeated calls, a different body for `--seed 7`, and schema validity of both.
- [ ] 2.6 Document the command, `Prefer` syntax and selection order in `docs/mock.md`.

## 3. Validation and security

- [ ] 3.1 Request validation (params 400, media type 415, body 422, up to 20 errors) with `jsonschema` and the explorer's 3.0→JSON Schema conversion, plus `--no-validate`; verify the missing-`q`, `{"name": 5}` and `text/plain` scenarios.
- [ ] 3.2 `--enforce-security` for API key, bearer, basic and OAuth2-as-bearer with `WWW-Authenticate`, optional expected values from `environments` secrets; verify the 401 and not-enforced-by-default tests.

## 4. Overrides, templates and faults

- [ ] 4.1 Parse `mocks/*.rman-mock.yaml` (match on method, path/glob, query, headers, body JSONPath; respond with status, headers, body/body_file) in file and declaration order; verify the `$.amount gt 10000` → 402 and undocumented `/health` tests.
- [ ] 4.2 Response templates for request path/query/header/body plus dynamic variables; verify the echo-`id` test and empty rendering + log for unresolved references.
- [ ] 4.3 `delay_ms`/`jitter_ms` (≤ 60000) and faults (`error_rate`, `drop_connection`, `slow_body_bps`) with seeded per-route RNG; verify the 2000 ms delay, 503 error-rate, dropped-connection and reproducible fault sequence tests.

## 5. Stateful mode

- [ ] 5.1 In-memory store keyed by the `api-exploration` resource graph, id generation per id schema, CRUD semantics and 404 for unknown ids; verify the create-read and delete-read tests.
- [ ] 5.2 `POST /__rustman/reset`, reserved `/__rustman/` prefix and the per-resource cap (10000 → 507); verify the reset and cap tests.

## 6. Developer experience

- [ ] 6.1 CORS default on for loopback, preflight handling before validation, `--cors/--no-cors`; verify the localhost:5173 preflight test.
- [ ] 6.2 Request log (human to stderr, `--jsonl` to stdout) with response source and redaction; verify the JSON-lines and redacted-Authorization tests.
- [ ] 6.3 `--history` integration tagging exchanges `mock`; verify `rustman history list` shows them.
- [ ] 6.4 Hot reload with `notify` (200 ms debounce) and `ArcSwap` state that keeps the stateful store; verify the add-operation-within-1 s test and that a broken save keeps old routes and logs file:line.
- [ ] 6.5 TUI mock pane (start/stop, live request log, response source) via the `add-terminal-ui` pane registry; verify with a ratatui test-backend snapshot.

## 7. Proxy, record and self-check

- [ ] 7.1 `--proxy-unmatched` via `rustman-http` with the webhook header-hygiene function and single-origin restriction; verify a test that an unmatched route reaches a local upstream and nothing else is contacted.
- [ ] 7.2 `--record <dir>` through the `collection-import` HAR IR (naming, dedup, secret extraction); verify the recorded file contains a `{{placeholder}}` instead of the token.
- [ ] 7.3 CI self-check: run the explorer's conformance probes against a mock of `fixtures/petstore.yaml`; verify zero conformance findings and exit code 0 in the workflow.
