# Tasks

## 1. Crate scaffold and page model

- [ ] 1.1 Create `crates/rustman-docs` and add `rustman docs build|serve --help` to `rustman-cli`; verify `cargo build` and help output.
- [ ] 1.2 Build the `DocSite` model from the workspace (sections in workspace order, hidden requests excluded, CommonMark descriptions) with the `Rendered` newtype created only through redaction; verify unit tests for ordering, hidden requests, and a compile-fail test that raw strings can't enter the model.
- [ ] 1.3 Add OpenAPI enrichment from the `openapi-import` snapshot (summaries, parameter descriptions, schemas, deprecated badge, request-file precedence); verify tests for each scenario.

## 2. Examples and redaction

- [ ] 2.1 Implement opt-in examples from `request-history` (latest successful exchange) and from snapshots when `snapshot-testing` is present (`--examples snapshot`), with `example_redact_paths` and 64 KB truncation; verify tests for not-opted-in, opted-in and truncation.
- [ ] 2.2 Enforce environment publishing rules (`publish_variables`, secrets rejected with exit 2, `--env`); verify tests for both spec scenarios.
- [ ] 2.3 Implement the post-build leak scan (plain, base64, URL-encoded) that deletes offending files and exits 1; verify with a malicious custom template fixture.

## 3. HTML renderer, themes and search

- [ ] 3.1 Implement HTML layout templates (embedded, `minijinja`) with no external references and no-JS readable navigation; verify a test that scans output for external `src`/`href`/`url()`, and an HTML snapshot test.
- [ ] 3.2 Implement dark/light themes with `prefers-color-scheme`, a stored toggle with fallback, and WCAG AA contrast; verify a contrast check in CI over the CSS tokens.
- [ ] 3.3 Implement the search index and inline search script; verify an index snapshot, a headless-browser test finding `users`, and a benchmark with 2,000 endpoints under 100 ms.
- [ ] 3.4 Implement the output manifest, `--force` and the non-empty-directory guard; verify tests for both build scenarios.

## 4. Code samples

- [ ] 4.1 Map `SampleRequest` onto the shared snippet engine from `add-runner-parity` (placeholder mode), using its built-in templates for curl, HTTPie, fetch, Python requests and Rust reqwest (JSON, form, multipart, file bodies); verify snapshot tests per language and body type, and that generated curl samples run against a local fixture server.
- [ ] 4.2 Implement `[docs].samples` selection and `template_dirs` overrides with a versioned template context; verify tests for the custom-language scenario and the version-mismatch fallback.

## 5. Markdown output and determinism

- [ ] 5.1 Implement the Markdown renderer (README index, GFM tables, `<details>` for long bodies, `--format both`); verify snapshot tests and the index scenario.
- [ ] 5.2 Enforce the determinism rules (sorting, no timestamps, `\n`, stable anchors, content-hashed assets); verify a golden test building twice and comparing hashes, run on Linux and Windows in CI.

## 6. Protocol reference pages

- [ ] 6.1 Add GraphQL schema reference pages from the `graphql-requests` schema cache and JSON-RPC method pages from OpenRPC documents in `jsonrpc-requests`, omitted when absent; verify fixture tests for present and absent cases.
- [ ] 6.2 Add generic pages for other request kinds; verify a `kind: grpc` fixture builds without error.

## 7. Preview server and docs

- [ ] 7.1 Implement `rustman docs serve` (loopback, `notify` watcher with 200 ms debounce, atomic swap, SSE live reload injected only in preview, error overlay, port-in-use exit 2); verify integration tests for reload within 1 s and the busy-port scenario.
- [ ] 7.2 Document `rustman docs` in the user guide (config keys, opting in examples, publishing to GitHub Pages); verify doc examples in a doc test.
