# Tasks

## 1. Crates and WIT contract

- [x] 1.1 Create the `crates/rustman-plugin` (host) and `crates/rustman-plugin-sdk` (guest) crates, and add `wasmtime` with the component model behind default cargo feature `plugins`. Verify that `cargo build` and `cargo build --no-default-features` both succeed on Linux, macOS and Windows CI.
- [x] 1.2 Write the `rustman:plugin@1.0.0` WIT package (worlds `auth`, `assertion`, `generator`, `transformer`, `importer`, `kind`, `report`, plus host imports). Verify that `wasm-tools component wit` parses it, and that a CI check fails on non-additive edits within major 1.

## 2. Manifest, declaration, pinning and lockfile

- [x] 2.1 Parse and validate `rustman-plugin.toml` (required fields, known extension points, semver, API major). Verify with unit tests covering the "Unknown extension point" and "Newer API major" scenarios.
- [x] 2.2 Load `[plugins.<name>]` from `rustman.toml`, ignore undeclared plugin directories, verify sha256, and read and write `rustman.lock`. Verify with tests for "Undeclared plugin files", "Tampered plugin" (exit code 2) and "No silent upgrade".
- [x] 2.3 Fetch URL sources (https only, 20 MiB cap, TLS on) into `.rustman/plugins/<sha256>.wasm`. Verify with a local HTTPS fixture test, plus the "Plain HTTP refused" test.
- [x] 2.4 Document the manifest and the `rustman.toml` plugin tables in `docs/plugins.md`. Verify that the documented example loads in a doc test.

## 3. Sandbox, grants and limits

- [x] 3.1 Implement the grant parser (`clock`, `random`, `env:`, `fs:read:`, `net:`, `secrets:read:`), with manifest cross-checks and rejection of wildcards and path escapes. Verify with unit tests for "Grant beyond the manifest", "Path escape" and "Wildcard host refused".
- [x] 3.2 Implement the host imports, with grant checks at call time and no ambient WASI. `fs-read` resolves canonical paths and refuses symlinks that point out of the workspace. Verify with an adversarial test plugin that attempts every denied operation and gets `capability_denied`, with no side effects.
- [x] 3.3 Route the host `http` import through `rustman-http` and the safety policy's `GovernedSender`, recording exchanges in history. Verify the "Granted host" and "Other host" scenarios with a local server.
- [x] 3.4 Pass secrets as `secret-ref` handles, gate `reveal()` on grants, and redact all plugin outputs and logs. Verify all three secrets scenarios, including that the echoed secret is absent from history and reports.
- [x] 3.5 Add epoch-interruption CPU limits and `StoreLimits` memory limits, with per-plugin overrides, maximums, and instance recreation after traps. Verify with loop and allocation-bomb test plugins: the "Infinite loop" scenario completes in under 500 ms and the run continues.

## 4. Trust

- [x] 4.1 Hash the plugin set over `(name, sha256, grants)`, store trust in `.rustman/trust.yaml`, and add the TUI trust prompt listing grants in plain language. Verify with TUI snapshot tests for "Teammate adds a plugin" and "Grants changed".
- [x] 4.2 Make headless commands refuse untrusted plugin sets, and add `--trust-plugins`. Verify the "CI run" (exit code 2, no request sent) and "CI run with opt-in" integration tests.
- [x] 4.3 Make agent sessions load only trusted plugin sets, and expose no plugin-management tools. Verify with an MCP integration test listing the tools and checking the startup log line.

## 5. Extension points

- [x] 5.1 Auth schemes (`auth: {type: plugin, ...}`), enforcing no method, host or body change. Verify with an HMAC test plugin that produces the expected headers, plus the host-rewrite rejection test.
- [x] 5.2 Assertion functions reported like built-in assertions, and plugin fault isolation mapped to assertion `error` or request error (exit code 3). Verify the "Custom business rule" and "Trap during auth" scenarios.
- [x] 5.3 Value generators `{{$<plugin>.<name>}}`, re-evaluated for each occurrence. Verify with a `faker` test plugin.
- [x] 5.4 Request and response transformers, with history storing both bodies. Verify the "Decrypt a response before assertions" scenario.
- [x] 5.5 Importers writing through host validation. Verify the "Plugin importer" and "Invalid output" scenarios (nothing written on error).
- [x] 5.6 Request kinds registered in the `workspace` kind registry, with schema validation, declarative TUI panes and reserved built-in names. Verify with a `soap` test plugin end to end, a TUI snapshot test, and the reserved-kind test.
- [x] 5.7 Report renderers for `--reporter` and `report render`. Verify that the output file bytes equal the plugin's return value.
- [x] 5.8 Document every extension point with an example in `docs/plugins.md`. Verify that each example plugin builds in CI.

## 6. CLI, SDK and harness

- [x] 6.1 Add `rustman plugin add|list|remove|verify|api|test|new`. Verify with CLI integration tests for "Add without grants" and "Verify in CI" (exit codes 0 and 2).
- [x] 6.2 Build the SDK bindings with `wit-bindgen` and a `plugin new` template for each extension point. Verify that CI scaffolds, builds to `wasm32-wasip2` and adds each template ("Scaffold builds").
- [x] 6.3 Build the host test harness with a fake clock, randomness, network and secrets, plus `rustman plugin test`. Verify the "Deterministic signing test" scenario: two runs are byte-identical and exit with code 0.

## 7. Review hardening

- [x] 7.1 Keep `fs:read` inside the workspace when the granted folder is itself a symlink: the canonical granted folder and the canonical target must both be under the canonical workspace root and outside `.rustman/`. Verify with a granted folder symlinked out of the workspace (`a_granted_folder_symlinked_out_of_the_workspace_is_refused`).
- [x] 7.2 Bound what a guest can make the host keep: at most 100 recorded refusals per instance, guest-supplied details cut to 256 bytes, secret names over 256 bytes get no handle, and at most 1024 resource handles. Verify with host-call loops using huge names.
- [x] 7.3 Open the trusted plugin set for TUI sends (`PluginGate::extensions`), with the environment's secrets and the network of the grants, and reuse compiled components across sends. Verify a TUI send of a plugin generator succeeds once trusted and is refused with a clear message without trust.
- [x] 7.4 Load plugins in `rustman bench` and `rustman history replay` under the headless trust rule (`--trust-plugins`). Verify both commands refuse an untrusted set (exit code 2) and use the plugins once trusted.
- [x] 7.5 Show the recorded outcome in `history list` for exchanges without an HTTP status (plugin request kinds).
- [x] 7.6 Fingerprint what a load actually read and refuse a load whose fingerprint is not the approved one. Verify a manifest edited between the trust check and the load is refused.
- [x] 7.7 Check the addresses a granted host name resolves to when a plugin request connects (an address guard on the engine's resolver), refusing loopback, private, link-local and unspecified addresses unless the grant names the address or `localhost`. Verify the classification and decision with injected addresses, and the guard on a real connection.
- [x] 7.8 Advertise report plugins in `run --reporter` and `report render --format` help only in a build with plugin support.
- [x] 7.9 Pin the `wasm-tools` CI installs, and build the scaffolded templates `--locked` against committed lockfiles.
- [x] 7.10 Name the per-call limit for what it measures: execution time (wall time minus host calls), in errors (`execution time limit exceeded`) and docs.

## Workflow follow-up

- Archive the change once implementation and review are complete (`openspec archive add-plugin-system`).
