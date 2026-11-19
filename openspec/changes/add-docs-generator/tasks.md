# Tasks

See design.md "Building on existing code" for the components each task reuses.

## 1. Crate scaffold and page model

- [ ] 1.1 Create `crates/rustman-docs` (depends on `rustman-core`, `rustman-http`, `rustman-explore`), add `[docs]` to `Settings`, and add `rustman docs build|serve --help` to `rustman-cli`; verify `cargo build`, help output, and no `unknown key 'docs'` warning.
- [ ] 1.2 Add the `docs:` request-file key (`example`, `hidden`, description) to every kind's model in `crates/rustman-core/src/model/` and to the writer, then build the `DocSite` model from the workspace (sections in workspace order, hidden requests excluded, CommonMark descriptions) with the `Rendered` newtype created only through `rustman_core::Redactor`; verify unit tests for ordering, hidden requests, a load/write round trip of `docs:`, and a compile-fail test that raw strings can't enter the model.
- [ ] 1.3 Add OpenAPI enrichment from the import snapshot under `.rustman/openapi/` (`rustman_explore::store`), matching requests to operations by their `openapi:operationId=` / `openapi:path=` tags as `rustman_explore::sync::local` does (summaries, parameter descriptions, schemas, deprecated badge, request-file precedence); verify tests for each scenario.

## 2. Examples and redaction

- [ ] 2.1 Implement opt-in examples from `request-history` (`crates/rustman-core/src/history/`, latest successful exchange) and from snapshots (`crates/rustman-core/src/snapshot/store.rs`, `--examples snapshot`), with `example_redact_paths` and 64 KB truncation; verify tests for not-opted-in, opted-in and truncation.
- [ ] 2.2 Enforce environment publishing rules (`publish_variables`, secrets rejected with exit 2, `--env`); verify tests for both spec scenarios.
- [ ] 2.3 Implement the post-build leak scan (plain, base64, URL-encoded) on `Redactor`'s encoded forms, moving the base64 forms of `crates/rustman-mcp/src/server/redact.rs` into `rustman-core` so the MCP server and docs share them; it deletes offending files and exits 1; verify with a malicious custom template fixture and the MCP redaction tests unchanged.

## 3. HTML renderer, themes and search

- [ ] 3.1 Implement HTML layout templates (embedded, `minijinja`) with no external references and no-JS readable navigation; verify a test that scans output for external `src`/`href`/`url()`, and an HTML snapshot test.
- [ ] 3.2 Implement dark/light themes with `prefers-color-scheme`, a stored toggle with fallback, and WCAG AA contrast; verify a contrast check in CI over the CSS tokens.
- [ ] 3.3 Implement the search index and inline search script; verify an index snapshot, a headless-browser test finding `users`, and a benchmark with 2,000 endpoints under 100 ms.
- [ ] 3.4 Implement the output manifest, `--force` and the non-empty-directory guard; verify tests for both build scenarios.

## 4. Code samples

- [ ] 4.1 Generate samples with the existing `rustman_http::snippet` engine in placeholder mode (`SnippetOptions::placeholders`, `http_entry` for GraphQL/JSON-RPC), using its built-in templates and the `curl`, `httpie`, `fetch`, `python`, `rust` ids (JSON, form, multipart, file bodies); no new sample model or templates are needed; verify snapshot tests per language and body type, and that generated curl samples run against a local fixture server.
  - Note (add-runner-parity 4.5): also add the test that `rustman docs` code samples and `rustman export snippet --placeholders` output match byte for byte; it closes runner-parity task 4.5.
- [ ] 4.2 Implement `[docs].samples` selection (any `Lang` id, e.g. `go`) and `template_dirs` overrides through a new override hook in `crates/rustman-http/src/snippet/` (scope per the design open question) with a versioned template context; verify tests for the custom-template scenario and the version-mismatch fallback.

## 5. Markdown output and determinism

- [ ] 5.1 Implement the Markdown renderer (README index, GFM tables, `<details>` for long bodies, `--format both`); verify snapshot tests and the index scenario.
- [ ] 5.2 Enforce the determinism rules (sorting, no timestamps, `\n`, stable anchors, content-hashed assets); verify a golden test building twice and comparing hashes, run on Linux and Windows in CI.

## 6. Protocol reference pages

- [ ] 6.1 Add GraphQL schema reference pages from the schema cache (`.rustman/graphql/schemas/`, `crates/rustman-graphql/src/cache.rs`) and JSON-RPC method pages from OpenRPC catalogs (`[jsonrpc.catalogs]` and `.rustman/cache/openrpc/`, `crates/rustman-jsonrpc/src/catalog.rs`), omitted when absent; verify fixture tests for present and absent cases.
- [ ] 6.2 Add generic pages for other request kinds (`mcp`, and unregistered kinds loaded as `OpaqueRequest` through `KindRegistry::with_opaque_fallback`); verify a `kind: grpc` fixture builds without error.

## 7. Preview server and docs

- [ ] 7.1 Implement `rustman docs serve` (loopback, a `notify` watcher with 200 ms debounce following `crates/rustman-mock/src/watch.rs`, atomic swap, SSE live reload injected only in preview, error overlay, port-in-use exit 2); verify integration tests for reload within 1 s and the busy-port scenario.
- [ ] 7.2 Document `rustman docs` in the user guide (config keys, opting in examples, publishing to GitHub Pages); verify doc examples in a doc test.
