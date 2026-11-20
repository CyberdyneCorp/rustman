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

### D9. Implementation notes (sections 1-3)
- **Embedded manifest.** A component may carry its manifest in a custom section named `rustman-plugin.toml`; a URL source must, since only the `.wasm` is downloaded. Embedded, the manifest is covered by the sha256 pin. A folder source names its component with the manifest's optional `component` key (default `plugin.wasm`).
- **WIT freeze.** The package lives in `crates/rustman-plugin-sdk/wit/`. Its surface (types, functions, world imports and exports) is frozen in `wit/frozen/1.0.0.txt`; a test fails on any frozen line that changes or disappears.
- **Plugin HTTP.** Redirects are not followed for plugin requests: the plugin sees the redirect, and its next request is grant- and policy-checked again. Time spent in host calls (network, files) does not count against the execution time budget (`cpu_ms`, which is wall time minus host-call time, not OS CPU time). A call is bounded in wall time by that budget plus 60 s, host calls included (`time limit exceeded`). Plugin requests go through an engine whose resolver checks every address a granted name resolves to, when the connection opens: a loopback, private, link-local or unspecified address is refused (`policy_denied: ...`) unless the grant names the address or `localhost` literally. Checking at connect time (an `AddressGuard` on the engine's DNS) rather than before the request closes the DNS-rebinding window; requests through an HTTP proxy are resolved by the proxy and not checked.
- **No WASI at all.** The linker holds only `rustman:plugin` imports. Components built for `wasm32-wasip2` with the Rust standard library may import WASI interfaces they never call; the SDK and template (task 6.2) must produce components without them, or the host must add deny-all stubs for those interfaces. Either way no ambient capability is granted.
- **Test plugins** are WAT core modules wrapped into components in-process with `wit-component` (`rustman-plugin`'s `test-fixtures` feature), so CI needs no `wasm32-wasip2` target and no checked-in binaries.

### D10. Implementation notes (sections 4-5)
- **Core seams.** `rustman-core::plugins` holds the `Extensions` trait (generators, auth signing, assertion functions, report rendering) and the `PluginGate` trait with the trust prompt data. The resolver, the assertions, the HTTP engine and the front-ends only see these traits, so the core, `rustman-http` and the TUI never depend on wasmtime; `rustman-plugin` implements both (`PluginRuntime`, `TrustGate`) and the CLI wires them.
- **Trust scope.** The trust hash is computed from the `[plugins.*]` tables alone, so checking trust reads `rustman.toml` and never downloads or runs anything. Approving re-hashes the tables and refuses when they changed while the prompt was shown.
- **Auth signing point.** A plugin scheme signs the request inside the engine's `prepare`, after the body and headers are built, so the plugin sees the exact bytes sent; signing happens once per send (redirect hops keep the signed headers).
- **Auth params.** `auth: {type: plugin, ...}` keeps every non-built-in key as a param. The auth map is read by hand (no derive) so unknown keys keep their source position and other types still refuse them.
- **Assertion exchange.** Plugin assertions receive the request's method, URL and headers (redacted), not its body.
- **Redaction of plugin output.** `Extensions::redactor` holds every secret of the plugin session; a run adds it to what every sink redacts, so a granted secret a plugin echoes (in a header of any name, a generated value, a transformed body or a kind's exchange) never reaches history or reports. Auth plugins may not set `Host` or the body-framing headers.
- **Trust scope, refined.** The trust hash also covers each plugin's `limits` and the hash of a path source's sidecar `rustman-plugin.toml` (an embedded manifest is covered by the pin).
- **Plugin network in real runs.** The CLI builds a `GovernedNetwork` for every session whose plugins hold `net:` grants: a `SafetyPolicy` whose allowlist is the granted origins plus `[safety].allowed_hosts` (its target, `https://plugins.invalid`, names no host), mutations allowed, destructive methods and production hosts refused; exchanges go to the workspace history.
- **Transformers.** Request transformers run in the engine's `prepare`, after the body and trace headers and before plugin auth; the host keeps `Host` and sets `Content-Length`. Response transformers run in the runner before assertions and captures; the record's `transform` keeps the body as received (history and the JSON report store both).
- **Kinds.** `PluginRuntime::new` asks each kind plugin for its kinds, refusing built-in names (`model::BUILTIN_KINDS`) and compiling each JSON Schema once. A workspace that declares plugins loads unknown kinds as opaque requests; `run` and `send` then check each against the loaded kinds and schemas before sending (`plugins::check_kinds`), and the runner executes them through `Extensions::execute_kind` with every field interpolated. The TUI asks `PluginGate::kinds` once the set is trusted and opens such requests in a declarative pane (`plugin_pane`).
- **Importers.** `rustman import <format> <src>` (a second positional after the format) reads the source through the plugin's `fs:read` grants, and `core::plugin_import` validates every returned file (paths, kinds, schemas, environments) before writing any.
- **Secret providers** (task 5.3 of `add-secret-providers`) are not an extension point of API 1; they can be added as a new interface within major 1.

### D11. Implementation notes (section 6)
- **SDK.** `rustman-plugin-sdk` generates its bindings with `wit-bindgen` (no std, async or bitflags): a `host` module for the imports, and one module per world behind a cargo feature of the same name, each with its `Guest` trait re-exported and a public `export!` macro. The `runtime` feature gives a `no_std` component its allocator (dlmalloc), panic handler and `cabi_realloc` (which `wasm32-wasip2` otherwise expects from the C library). A plugin built this way imports only `rustman:plugin` interfaces, so the host keeps linking no WASI at all (D9) and needs no deny-all stubs.
- **Template.** `plugin new` writes a standalone cargo project (its own `[workspace]`, `cdylib`, `panic = "abort"`) whose manifest names `target/wasm32-wasip2/release/<crate>.wasm` as its component, so the folder is a valid path source once built. Until the SDK is published, the template takes it from the Rustman repository (a git dependency); `plugin new --sdk <path>` uses a local copy. CI (`scripts/check_plugin_templates.py`, Linux) scaffolds, builds, tests and adds every template against the repository's SDK with `--sdk`; the template sources live in `crates/rustman-plugin/templates/`.
- **CLI edits.** `plugin add` and `plugin remove` edit `rustman.toml` as text (append a table, drop a table and its sub-tables), keeping comments and line endings; every edit is re-parsed and refused unless it declares exactly what was asked. A name or source already declared is refused before any download (no in-place upgrade), and `plugin add` compiles the component (no code runs) so a WASI-importing build is refused at add time. `plugin verify` re-hashes before parsing anything, then loads with downloads allowed and rewrites `rustman.lock` only when it changed.
- **Harness.** The harness lives in the host crate (`rustman-plugin::harness`, behind `rustman plugin test`), not in the guest SDK, because it needs wasmtime. Cases are TOML: shared `[fakes]` (clock, seed, env, secrets, canned HTTP responses) plus per case its grants, exactly one call and `expect` (exact output lines) or `expect_error`. Random bytes are SplitMix64 from the seed; the process environment and network are never reached. Output is redacted of the fake secrets after the expectations are compared.
- **Secret providers.** Still not an extension point of API 1 (D10); a provider interface can be added within major 1, and the SDK would gain a matching world feature.

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
