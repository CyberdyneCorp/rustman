# Proposal

## Why

GraphQL APIs are first-class citizens next to REST, but treating them as plain HTTP POSTs throws away what makes them productive: the schema. Users need schema-aware editing, validation before send, subscriptions and uploads — the things that make Postman/Altair worth using — inside the same local-first workspace and runner introduced by `bootstrap-core-engine`.

## What Changes

- `kind: graphql` request files become executable (the kind is only reserved by `bootstrap-core-engine`): endpoint, document, `operationName`, variables, headers, auth — all with `{{variable}}` interpolation from `environments`.
- Queries and mutations over HTTP POST (`application/json`), with opt-in GET for queries.
- Introspection with an on-disk schema cache (`.rustman/graphql/schemas/`), explicit refresh, and loading a schema from an SDL/JSON file when introspection is disabled.
- Schema explorer data (types, fields, arguments, enums, directives, deprecations) and completion data consumed by the TUI.
- Client-side validation of the operation against the cached schema before send, with line/column error locations.
- Subscriptions over `graphql-transport-ws`, detection of legacy `subscriptions-transport-ws`, and GraphQL over SSE.
- Multipart file uploads (graphql-multipart-request-spec) and Automatic Persisted Queries (APQ).
- Response rendering that keeps partial `data` and `errors` side by side; assertions on `data` / `errors` paths via the `assertions` capability.
- CLI `rustman graphql introspect` and `rustman graphql query`; `rustman run` executes graphql request files; a TUI GraphQL pane (layout owned by `add-terminal-ui`).

## Capabilities

### New Capabilities

- `graphql-requests`: defining, validating and executing GraphQL operations (queries, mutations, subscriptions, uploads, APQ), schema introspection and caching, schema explorer and completion data, and GraphQL-specific response handling and CLI commands.

### Modified Capabilities

None (greenfield; specs from `bootstrap-core-engine` are not archived yet — this change builds on `workspace`, `environments`, `http-requests`, `assertions`, `cli-runner` and `request-history` without altering their requirements).

## Out of Scope

- GraphQL server mocking or schema-based fake data generation.
- Federation/supergraph composition tooling and schema diffing/linting (candidate for a later change).
- Batched operations (array payloads) and `@defer`/`@stream` incremental delivery (rendered as raw responses only).
- Code generation (typed clients).

## Impact

- New crate `crates/rustman-graphql`; TUI pane in `crates/rustman-tui`; subcommands in `crates/rustman-cli`.
- Reuses the `rustman-http` transport (TLS, proxies, auth, timings) for HTTP operations.
- New dependencies: `apollo-compiler` (parse/validate), `tokio-tungstenite` (WebSocket handshake and framing, no TLS features). SSE is parsed in-crate. Rationale in design.md.
- Extends the request-file schema for `kind: graphql` (version stays `1`; fields are additive).
