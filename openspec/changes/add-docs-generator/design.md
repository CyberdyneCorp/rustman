# Design

## Context

See proposal.md for the motivation. Everything the docs need already exists as data in other capabilities: the workspace model and order (`workspace`), variable layers and the redaction step (`environments`), exchanges (`request-history`), the OpenAPI import snapshot (`openapi-import`), and optional schema caches (`graphql-requests`, `jsonrpc-requests`). This change is a read-only transformer from that data into files.

## Goals / Non-Goals

**Goals:**
- Output that is reviewable in Git: deterministic and stable when unrelated things change.
- Zero external network references, so the site works offline and on air-gapped intranets.
- Safe by construction: no secret can reach a page, even through a custom template.

**Non-Goals:**
- A themeable site framework or a plugin API for renderers (template overrides are enough for v1).
- Server-side rendering or any runtime component in the output.

## Decisions

### D1. Two-stage pipeline: page model → renderers
`rustman-docs` first builds a `DocSite` model (sections, endpoints, examples, samples, search entries). Every string value in it passes the redaction step at construction time, with values wrapped in a `Rendered` newtype that can only be created through redaction. The HTML and Markdown renderers consume only the model, so neither can see raw workspace values.
*Alternative rejected:* rendering templates straight from workspace structs. Every template author would then be responsible for redaction.

### D2. Templates with `minijinja`
Code samples, and the page layout overrides, use `minijinja` (pure Rust, Jinja2 syntax, sandboxed: no filesystem or network access from templates). Built-in templates are embedded with `include_str!`. User templates in `[docs].template_dirs` override them by name.
*Alternatives rejected:* `tera` (larger, with unnecessary features) and `handlebars` (logic-less, which makes multi-language code samples awkward).

### D3. Post-build leak scan as a second line of defense
After writing, the builder scans every output file for each known secret value and its base64 and URL-encoded forms, reusing the scanner from `agent-mcp-interface`'s second redaction pass if it's present, or the same algorithm from `rustman-core`. Any hit fails the build (exit 1) and deletes the offending file. This covers custom templates that bypass the model, for example by printing environment variables passed through a future extension.

### D4. Search: prebuilt index, tiny inline engine
At build time we produce `search-index.json` (tokenized names, paths, descriptions, parameter names) plus a small dependency-free JS file (under 10 KB) doing prefix and fuzzy matching. 2,000 endpoints produce an index of roughly 300 KB.
*Alternatives rejected:* `lunr.js` or `pagefind`. lunr would be vendored JS we must ship and audit, and pagefind needs its own binary and WASM. The spec's 100 ms budget is reachable with a simple engine at this scale.

### D5. Determinism rules
- Sort by workspace order, then by path, using byte-wise comparison.
- No timestamps. Example recording dates come from the exchange, not the build.
- Write `\n` line endings and UTF-8 without a BOM.
- Use stable ids for anchors (a slug of the path, with a numeric suffix on collision assigned in order).
- Name asset files after their content hash (`app.<hash8>.css`) so caches bust correctly.

A golden test builds a fixture workspace twice, and on Linux and Windows in CI, and compares hashes.

### D6. Preview server
`rustman docs serve` uses `hyper` on `127.0.0.1` with `notify` file watching (debounced 200 ms), rebuilding into a temp dir and swapping atomically. Live reload uses an SSE endpoint (`/__rustman/reload`) and a script injected only in preview mode, never in `docs build` output.
*Alternative rejected:* WebSocket reload. SSE is simpler with hyper and needs no extra crate.

### D7. Code-sample generation
Samples come from the shared snippet engine in `rustman-http` (from `add-runner-parity`), called in placeholder mode, so docs and `rustman export snippet` never drift. A single neutral `SampleRequest` (method, URL template, headers, body by type) feeds every template. Variables stay as `{{name}}` in curl and HTTPie, and become language-appropriate placeholders in code (`BASE_URL` constants), consistently across languages. Multipart and file bodies render with each language's idiomatic file API.

### D8. Markdown flavor
GitHub-flavored Markdown with tables and fenced code. Long bodies go into a `<details>` block so the tables stay readable on GitHub.

## Risks / Trade-offs

- [Opted-in examples from history may still contain personal data that isn't a declared secret] → Examples are opt-in per request. The build prints a summary of which requests published examples, and `[docs].example_redact_paths` lets users mask JSONPaths (e.g. `$.users[*].email`).
- [Large workspaces make huge sites] → Endpoint pages are only generated for visible requests. Example bodies are truncated at 64 KB with a note.
- [Template overrides break between versions] → Templates receive a versioned context (`docs_context_version`). A mismatch warns and falls back to the built-in template.

## Migration Plan

Additive: new subcommands and optional config keys only.

## Open Questions

- Whether to offer an OpenAPI export (`rustman docs openapi`) generated from the workspace in a later change. It reuses the page model and doesn't affect these specs.
