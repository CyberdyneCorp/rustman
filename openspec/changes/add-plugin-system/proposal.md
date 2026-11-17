# Proposal

## Why

Teams keep hitting gaps that the built-in engine shouldn't own: in-house auth (HMAC, SigV4-style signing), domain-specific assertions, custom importers, niche protocols, company report formats. Without an extension mechanism, every gap turns into a feature request or a fork. Plugins have to be safe by construction, because workspaces are shared through Git and a plugin pulled in by a teammate's commit must not be able to read your disk, your environment or your secrets.

## What Changes

- Add sandboxed **WASM component-model plugins**, hosted with `wasmtime` behind WIT-defined interfaces, with a versioned plugin API.
- Extension points: auth schemes, assertion functions, value generators (dynamic variables), body and response transformers, importers, request kinds (registered in the `workspace` kind registry, with a declarative TUI pane), and report renderers.
- A plugin manifest, `rustman-plugin.toml`, declares name, version, API version, extension points and the capabilities it requests.
- **Capability-based sandbox.** A plugin gets no filesystem, network, environment or clock unless the workspace grants each capability. Network grants are scoped to hosts and go through the safety policy. Secrets reach a plugin only with an explicit `secrets:read:<name>` grant.
- Resource limits on every call (default 100 ms CPU, 64 MiB memory). Exceeding a limit is a plugin error, never a crash.
- Installation from a local path or URL, pinned by sha256 in `rustman.toml` and recorded in `rustman.lock`. No auto-update.
- CLI: `rustman plugin add|list|remove|verify|new`.
- Workspace trust: the first load of a workspace with plugins asks the human. Headless runs need `--trust-plugins`.
- A plugin SDK crate (`rustman-plugin-sdk`), a project template, and a host test harness for deterministic plugin tests.
- Agents can't install, grant or trust plugins.

Out of scope: a public plugin registry or marketplace; native (non-WASM) plugins; plugins that draw arbitrary TUI widgets (only declarative panes); hot-reloading plugins while a run is in progress; scripting languages (Lua, Rhai, JavaScript) inside request files.

## Capabilities

### New Capabilities
- `plugins`: plugin packaging, installation and pinning, workspace trust, the capability sandbox and resource limits, the extension points and how each integrates with the engine, API versioning, and the SDK and test harness contract.

### Modified Capabilities
None. Plugins hook into extension points that already exist (the `workspace` kind registry, `assertions`, `environments` dynamic variables, `http-requests` auth, `test-reports` renderers) without changing their requirements.

## Impact

- New crates: `rustman-plugin` (host, built on `wasmtime`) and `rustman-plugin-sdk` (guest SDK, published separately).
- `rustman-core` gains plugin-backed registries for kinds, auth schemes, assertions, generators and transformers.
- `rustman.toml` gains `[plugins.<name>]` tables. A new tracked file `rustman.lock` is added. Trust decisions live in the git-ignored `.rustman/trust.yaml`, the file `mcp-client` already uses.
- `rustman-cli` gains the `plugin` subcommand and `--trust-plugins`.
- Binary size grows because of `wasmtime` (Cranelift). It stays pure Rust with no system libraries.
