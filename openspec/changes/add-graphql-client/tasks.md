# Tasks

## 1. Crate and request model

- [x] 1.1 Create `crates/rustman-graphql` with `apollo-compiler` pinned; verify `cargo build -p rustman-graphql` on Linux/macOS/Windows CI.
- [x] 1.2 Extend the request-file loader for `kind: graphql` (endpoint, document/document_file, operation_name, variables, headers, auth, transport, method, apq, schema_file, ws_protocol, max_events, duration); unit tests for valid files and the `exactly one of document or document_file` error.
- [x] 1.3 Interpolate `{{vars}}` in endpoint, headers and variable string values but never in the document; tests for both scenarios in the spec.

## 2. Schema: introspection, cache, file

- [x] 2.1 Implement the introspection query and introspection-JSON → schema conversion; golden tests against fixture introspection results (GitHub, Shopify-sized samples).
- [x] 2.2 Implement the cache at `.rustman/graphql/schemas/<hash>.json` with endpoint, fetch time and SDL hash; tests that send never triggers introspection when a cache exists.
- [x] 2.3 Support `schema_file` (SDL and introspection JSON) taking precedence over the cache; test the `introspection failed ... set schema_file` hint using a fake server that rejects introspection.

## 3. Validation, deprecations, completion, explorer

- [x] 3.1 Pre-send validation with `line:column` diagnostics and syntax-only mode without schema; tests for unknown field (`1:17`) and missing brace.
- [x] 3.2 Operation selection and GET-mutation rejection; tests for `document has 2 operations` and `mutations cannot use GET`.
- [x] 3.3 Variables validation against variable definitions; test `variable $id of type ID! is required`.
- [x] 3.4 Deprecation warnings with reason; test with a deprecated field fixture.
- [x] 3.5 Explorer data API (types, fields, args, enums, directives, deprecations); snapshot tests with `insta`.
- [x] 3.6 Completion engine over the tolerant CST (fields, args, enum values, variables, directives, fragments); table-driven tests with cursor markers.

## 4. Execution over HTTP

- [x] 4.1 POST/GET execution through `rustman-http` with the `Accept` header in the spec; tests against a local hyper test server asserting the exact payload.
- [x] 4.2 Response parsing into data/errors/extensions with `ok`/`partial`/`errors` status and non-GraphQL body detection; tests for partial data and HTML 502.
- [x] 4.3 Multipart uploads per graphql-multipart-request-spec with files read at send time by the shared `rustman-http` encoder (streaming deferred, design D5) and missing-file error; test asserts `operations`, `map` and part MIME type.
- [x] 4.4 APQ hash-first with single retry on `PersistedQueryNotFound`; test counts two requests and the `APQ miss (registered)` marker.

## 5. Subscriptions

- [x] 5.1 `graphql-transport-ws` client state machine (init, ack timeout 10 s, subscribe, next, error, complete); tests against a fake WS server including the ack-timeout scenario.
- [x] 5.2 Legacy `graphql-ws` fallback and `ws_protocol` override; test with a legacy-only fake server and UI label.
- [x] 5.3 GraphQL over SSE (distinct connections); test with a fake SSE server ending on `complete`.
- [x] 5.4 Headless bounds (`max_events` 10, `duration` 30 s, CLI overrides); test that 50 server events stop at 10.

## 6. Assertions, history, CLI

- [x] 6.1 Register `no_errors`, `errors_count`, `error_message_contains`, `error_code` and `data` JSONPath assertions plus captures from `data`; tests in the runner for fail and capture scenarios.
- [x] 6.2 Record GraphQL exchanges in `request-history` (operation name/type, redacted variables, status, first 100 events); test redaction of a secret variable.
- [x] 6.3 `rustman graphql introspect [--out]` and `rustman graphql query` with exit codes 0/1/transport; CLI tests with `assert_cmd`; document both commands in `docs/graphql.md`.
- [x] 6.4 Enable `kind: graphql` in `rustman run`; integration test running a collection mixing http and graphql requests.

## 7. TUI pane

- [x] 7.1 GraphQL pane (document editor with highlighting, diagnostics, completion popup, variables editor, explorer, Data/Errors/Extensions/Events tabs) on the `add-terminal-ui` framework; TUI snapshot tests with ratatui `TestBackend`, including the inline-diagnostic scenario.
- [x] 7.2 Refresh-schema action and subscription start/stop keybindings documented in the TUI help overlay; verify via snapshot of the help screen.
