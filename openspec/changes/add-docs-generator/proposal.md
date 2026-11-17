# Proposal

## Why

A Rustman workspace already describes an API precisely: endpoints, parameters, headers, bodies, auth and real recorded responses. Teams still maintain separate docs by hand, and those drift. Generating docs from the workspace keeps them truthful. Using real (redacted) examples makes them more useful than schema-only docs.

## What Changes

- New `rustman docs build [--out site/] [--format html|markdown|both]`. It generates a static, self-contained, offline site from the workspace: collections become sections, requests become endpoint pages.
- Endpoint pages show method, URL template, parameters, headers, body, auth scheme and assertions as tables. When an imported OpenAPI document is available, its descriptions and schemas are merged in.
- Real examples are taken from `request-history` (or from stored snapshots when `snapshot-testing` is present). Each request opts in individually, and examples are always redacted.
- Code samples are generated from built-in templates (curl, HTTPie, JavaScript `fetch`, Python `requests`, Rust `reqwest`), and users can override them with their own templates.
- Client-side search, a dark/light theme, and **deterministic output**, so the generated site can be committed and reviewed in Git.
- Environments are rendered as variable names only (`{{base_url}}`), unless a variable is explicitly marked publishable.
- `rustman docs serve` runs a local preview with live reload.
- GraphQL schema reference pages and JSON-RPC method pages (from OpenRPC when available) are added when those request kinds are present in the workspace.

## Capabilities

### New Capabilities
- `api-docs-generation`: static HTML and Markdown API documentation generated from a workspace, with redacted real examples, OpenAPI enrichment, code samples, client-side search, deterministic output, and a live-reload preview server.

### Modified Capabilities
None. It reads `workspace`, `environments` (redaction, variable layers), `request-history`, `openapi-import` (stored import snapshot), and, when present, `graphql-requests` (schema cache), `jsonrpc-requests` (OpenRPC documents) and `snapshot-testing`, all unchanged.

## Out of scope

- Hosting or publishing the site (users deploy `site/` to GitHub Pages, S3 or similar themselves).
- A "try it" console that sends requests from the browser.
- Docs for gRPC, WebSocket/SSE and message-broker kinds in v1. Their pages show the request-file fields only, as a generic table.
- Translating docs or managing doc versions.

## Impact

- New crate `crates/rustman-docs` (page model, renderers, search index, templates). It depends on `rustman-core` and `rustman-report` (shared redaction helpers), and optionally reads the schema caches of the GraphQL and JSON-RPC crates.
- New CLI subcommands `rustman docs build` and `rustman docs serve` (the preview server uses `hyper`, bound to loopback).
- New optional workspace keys: `[docs]` in `rustman.toml` (title, logo, base path, which examples are publishable, extra template dirs), and `docs:` in request files (`example: true`, `hidden: true`, extra Markdown description).
- No new runtime dependency for readers of the output: the site is plain HTML, CSS and a small inline JS file.
