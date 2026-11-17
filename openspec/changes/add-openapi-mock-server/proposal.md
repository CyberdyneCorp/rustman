# Proposal

## Why

Frontend and integration work often stalls until the backend exists, and testing failure paths (timeouts, 500s, malformed data) against a real backend is hard. Rustman already parses OpenAPI for import and exploration. Serving that same document as a realistic, validating mock costs little and closes the loop. You design the spec, mock it, build against it, then explore the real backend with the same spec. Prism does this today, but it needs Node and lives outside your API workbench.

## What Changes

- `rustman mock serve <openapi> [--port 4010] [--host 127.0.0.1]` serves the document's operations over HTTP using `hyper`. It binds to loopback by default; a non-loopback address needs `--allow-remote`.
- Routing comes from OpenAPI paths with path-parameter matching. Static segments win over templated ones, and unknown routes get `404` with a problem+json body.
- Response selection follows this order:
  1. the `Prefer` header (`Prefer: code=404, example=notFound, dynamic=true`, compatible with Prism)
  2. a hand-written override
  3. the lowest documented 2xx with its first named example
  4. data synthesized from the schema with a deterministic seed
- Requests are validated against the document (path, query, header and cookie parameters, body schema, content type). Failures get `400`, `415` or `422` with `application/problem+json` details. `--no-validate` turns this off.
- `--enforce-security` makes missing credentials for a declared security scheme return `401`.
- Hand-written mocks in `mocks/*.rman-mock.yaml`:
  - match on method, path, query, headers and body JSONPath;
  - respond with a status, headers and a body template that can use request values;
  - add delay and jitter;
  - inject faults: error rate, dropped connection, slow body.
- Optional `--stateful` mode keeps an in-memory CRUD store for resources inferred like the explorer's dependency discovery: `POST` creates, `GET` returns, `PUT`/`PATCH` update, `DELETE` removes. `POST /__rustman/reset` clears it.
- CORS for browser development (`--cors`, on by default for loopback).
- A request log streams as JSON lines (`--jsonl`), appears in a TUI mock pane, and can be saved to `request-history`.
- Changes to the spec or mock files reload automatically. If a change fails to parse, the server keeps serving the previous version.
- Optional `--proxy-unmatched <url>` forwards unmatched routes to a real upstream, with an optional `--record` that saves those exchanges as request files.
- Self-check: `add-openapi-explorer` can run against the mock (`rustman explore run --target http://127.0.0.1:4010`) to check that the document is consistent in CI.

## Capabilities

### New Capabilities

- `mock-server`: serves an OpenAPI document (plus hand-written overrides) as a validating, optionally stateful HTTP mock with fault injection, hot reload and request logging.

### Modified Capabilities

None. Reuses `openapi-import` parsing and `$ref` resolution, `api-exploration` dependency discovery for stateful resources, `environments` secrets for `--enforce-security` credentials, `request-history` for logs, and the `webhook-inspector` hyper/limits conventions, without changing their requirements.

## Out of Scope

- gRPC, GraphQL and AsyncAPI mocking (separate future changes).
- HTTPS termination on the mock (users can front it with a proxy).
- Persisting stateful data across restarts.
- Hosting or sharing mocks publicly.

## Impact

- New crate `crates/rustman-mock` (hyper 1.x server), depending on `rustman-explore` (OpenAPI model, schema synthesis, dependency graph) and `rustman-core`.
- New CLI subcommand `rustman mock serve`; a TUI mock pane registered through the `add-terminal-ui` pane registry.
- New workspace convention: the `mocks/` directory for `*.rman-mock.yaml` files.
