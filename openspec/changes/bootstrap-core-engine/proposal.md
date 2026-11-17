# Proposal

## Why

Rustman has no code yet. Every later change (TUI, GraphQL, MCP client, OpenAPI exploration, agent interface, viz companion) depends on the same foundation: a Git-friendly workspace format, environments with safe secret handling, an HTTP engine, declarative assertions, a headless runner and an exchange history. Building that foundation first, and once, keeps the three front-ends (TUI, CLI, MCP server) as thin adapters over a single engine, as `openspec/config.yaml` requires.

## What Changes

- Cargo workspace scaffold with the crate layout from `openspec/ROADMAP.md` (the core, http and cli crates plus the shared report types this change needs), and CI covering fmt, clippy, tests on Linux/macOS/Windows, and `openspec validate`.
- **Workspace format**: a `rustman.toml` root marker and settings file, one request per `*.rman.yaml` file, nested collections with explicit ordering, and `rustman init`.
- **Environments and variables**: named environments, a documented variable precedence, dynamic variables (`{{$uuid}}`, `{{$timestamp}}`, `{{$randomInt}}`, ...), secrets stored in the OS keychain with a fallback to a git-ignored file, a global redaction invariant, and a hard error on unresolved variables.
- **HTTP engine**: HTTP/1.1 and HTTP/2, timeouts, redirect policy, TLS verification on by default, custom CA, mTLS, proxies, a cookie jar per environment, auth helpers (basic, bearer, API key, OAuth2 client-credentials and auth-code + PKCE with a token cache), a timing breakdown, a response body cap with spill-to-disk, binary bodies, curl import and copy-as-curl.
- **Assertions and captures**: declarative checks (status, header, JSONPath, JSON Schema, body contains/regex, latency) and captures that chain values between requests.
- **CLI runner**: `rustman send` and `rustman run <path>` with `--env`, `--var`, `--bail` and `--parallel`; pretty/JSON/JUnit reporters; documented exit codes; `NO_COLOR` and non-TTY behavior.
- **Request history**: redacted exchanges in `.rustman/history/`, retention (500 by default), and search.

## Capabilities

### New Capabilities

- `workspace`: the on-disk layout, the `rustman.toml` settings, the request-file schema, collection ordering and `rustman init`.
- `environments`: environments, variable resolution and precedence, dynamic variables, secrets storage and redaction.
- `http-requests`: executing `kind: http` requests, covering the transport, TLS, proxies, cookies, auth helpers, timings, body handling and curl interop.
- `assertions`: declarative response assertions and captures that feed later requests.
- `cli-runner`: the headless `rustman send` / `rustman run` commands, reporters, exit codes and terminal behavior.
- `request-history`: recording, retention, redaction and search of past exchanges.

### Modified Capabilities

None. This is a greenfield project.

## Out of Scope

- The TUI (`add-terminal-ui`), GraphQL execution (`add-graphql-client`) and MCP execution (`add-mcp-client`). This change only reserves the `graphql` and `mcp` request kinds in the file schema, and the runner reports them as unsupported.
- OpenAPI import and exploration, HTML/Markdown reports, the agent MCP server, the webhook inspector and the viz companion.
- Scripting languages (pre-request or test scripts). Assertions are declarative only.
- WebSocket, gRPC, SSE streaming viewers, and HTTP/3.
- Cloud sync, accounts and telemetry, which are excluded permanently by project constraints.

## Impact

- New repository content: `Cargo.toml` workspace, `crates/rustman-core`, `crates/rustman-http`, `crates/rustman-report` (run-report types and the json/junit writers only), `crates/rustman-cli`, `.github/workflows/ci.yml`, `rust-toolchain.toml`, `README.md` and `docs/`.
- New dependencies: tokio, hyper/hyper-util with a custom timing connector, rustls, clap, serde, serde-saphyr (YAML), toml, keyring, serde_json_path, jsonschema and quick-junit. design.md gives the rationale for each and the alternatives that were rejected, including reqwest and serde_yaml.
- It defines the request-file format that every later change extends. Changing it later means a migration, so this change fixes the format at `version: 1`.
