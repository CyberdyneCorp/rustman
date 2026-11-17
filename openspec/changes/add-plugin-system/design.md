# Design

## Context

Plugins plug into extension points that other changes already define: the request-kind registry (`workspace`), auth (`http-requests`), dynamic variables (`environments`), `assertions`, `test-reports` renderers, the safety policy (`api-exploration`) and workspace trust (`.rustman/trust.yaml`, introduced by `mcp-client`). Workspaces travel through Git, so a plugin has to be treated as untrusted code. See proposal.md for the motivation.

## Goals / Non-Goals

**Goals:**
- A plugin can't touch anything it wasn't granted, and the grants are visible in `rustman.toml` code review.
- Plugins are reproducible: the same pinned bytes give the same behavior on Linux, macOS and Windows, without a per-platform build.
- A misbehaving plugin degrades a single operation, never the process.

**Non-Goals:**
- Running untrusted plugins with zero prompts.
- Performance parity with native code. Plugin calls are expected to be small (sign, check, transform).
- A plugin registry service.

## Decisions

### D1. WASM component model on `wasmtime` (rejected: Extism, Lua/Rhai, native dylibs)
- **`wasmtime`:** maintained by the Bytecode Alliance, pure Rust with the Cranelift JIT and no system libraries. It supports the component model and WASI Preview 2, with fuel, epoch interruption and memory limits built in.
- **WIT interfaces** give typed, versioned contracts per extension point, and plugins can be written in Rust, Go, JS or Python through component tooling.
- **Extism** (rejected) adds a second ABI layer, its own host-function model and an extra runtime dependency. Its sandboxing is coarser (manifest-level allowed hosts and paths), while we need grants per call routed through our safety policy.
- **Lua/Rhai** (rejected) are easy to embed, but sandboxing an interpreter is a long-tail security problem, and scripts in request files would undo the "declarative assertions, no scripting language" decision of the core.
- **Native dylibs** (rejected) can't be sandboxed and would need one build per platform.

### D2. One WIT package per API major: `rustman:plugin@1.x`
- Each extension point is a WIT world: `auth`, `assertion`, `generator`, `transformer`, `importer`, `kind` and `report`.
- Host imports (`http`, `clock`, `random`, `env`, `fs-read`, `secrets`, `log`) are always linked. Each checks its grant at call time and returns `capability-denied`.
- This gives one stable linker and clear error messages, instead of link-time failures that are hard to explain.
- The minor version evolves additively, and the host keeps adapters for older minors.

### D3. No ambient WASI
- We don't link `wasi:filesystem`, `wasi:sockets` or `wasi:cli/environment` preopens.
- `fs:read` is served by our own `fs-read` import, which resolves canonical paths, refuses anything outside the workspace or inside `.rustman/`, and refuses symlinks that point out of the workspace.
- `net` goes through a host `http` import backed by `rustman-http` and the `GovernedSender` from the safety policy (D3 of `add-openapi-explorer`). Every redirect hop is checked too.

### D4. Limits through epoch interruption plus `StoreLimits`
- A background ticker bumps the engine epoch every 10 ms. Each call sets a deadline of `cpu_ms / 10` ticks.
- Epoch interruption is cheaper than fuel metering, and it's what Wasmtime recommends for wall-clock budgets.
- `StoreLimits` caps linear memory and tables. One `Store` per instance, recreated after any trap.
- Plugin instances are pooled per plugin, and calls into one instance are serialized.

### D5. Trust and integrity
- The pinned sha256 of the `.wasm` file goes into `rustman.toml` and `rustman.lock` (tracked). The cache lives in `.rustman/plugins/` (git-ignored).
- The trust record in `.rustman/trust.yaml` is keyed by a hash over the sorted `(name, sha256, grants)` of every plugin. Changing any of them re-prompts.
- This mirrors the stdio-server trust in `mcp-client`, so users see one trust model.

### D6. Secrets as handles
- Values pass into plugins as `secret-ref` resources. Calling `reveal()` succeeds only when the plugin holds `secrets:read:<name>`.
- Everything a plugin returns, logs or reports is run through the redaction step of `environments` with the session's secret set. A plugin echoing a value can't leak it.

### D7. Declarative TUI panes for plugin kinds
- `kind` plugins return a pane descriptor: a list of fields with an editor type (text, code with a language, key-value, select) plus response views (raw, JSON tree, table).
- `tui` renders the descriptor with its existing widgets. Plugins never draw.

### D8. SDK crate is separate
- `rustman-plugin-sdk` is generated with `wit-bindgen` and published independently.
- It's versioned with the plugin API major, so plugin authors don't depend on the whole Rustman workspace.

## Risks / Trade-offs

- [`wasmtime` adds about 10–15 MB to the binary and slows compile times] → Plugin support is a default cargo feature (`plugins`) that can be disabled. Compiled modules are cached per sha256 in `.rustman/plugins/`.
- [JIT unavailable on some hardened platforms] → Wasmtime's Pulley interpreter is a fallback, behind a config switch.
- [A granted plugin still sees what it's granted, e.g. a secret] → Grants are explicit, reviewable in Git, and shown in the trust prompt. We never offer wildcards.
- [Users click through trust prompts] → The prompt lists each grant in plain language, and network grants show the exact hosts.
- [WIT/component tooling churn] → The API major is pinned to a WIT package version, and conformance tests run against the SDK template in CI.

## Migration Plan

Additive. Workspaces without `[plugins]` behave exactly as before. `rustman init` adds `.rustman/plugins/` coverage through the existing `.rustman/` ignore line. Rollback means removing the `[plugins.*]` entries.

## Open Questions

- Whether to also accept signed plugins (Sigstore/cosign) in addition to sha256 pins. This would only add verification, not change the pinning model.
