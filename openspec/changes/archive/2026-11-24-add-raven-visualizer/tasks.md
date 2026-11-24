# Tasks

## 1. Viz protocol crate

- [x] 1.1 Create `crates/rustman-viz-protocol` (serde only, no I/O) with the message enum (`hello`, `hello_ack`, `hello_reject`, `open_view`, run/exploration events, `finding`, `load_report`, `inspect_response`, `blob_ref`, `error`, `close`) and the `rustman-viz/1` constant; verify with round-trip unit tests and a test that unknown fields and unknown `type` values decode without error.
- [x] 1.2 Add a line-framing codec enforcing the 16 MiB cap and `blob_ref` spill-over for larger payloads; verify with tests at 16 MiB−1 / +1 bytes.
- [x] 1.3 Add a `cargo fuzz` target for the decoder; verify it runs for 60 s in CI without panics. (Target in `crates/rustman-viz-protocol/fuzz/`; the `fuzz-viz-protocol` CI job ran 1.78M inputs in 61 s without a panic in PR #54.)
- [x] 1.4 Document the protocol (messages, versioning rules, exit codes) in `docs/viz-protocol.md`; verify with a doc test that parses every example message in the doc.

## 2. Producer side in rustman

- [x] 2.1 Implement discovery (`RUSTMAN_VIZ` → `PATH` → sibling) with `--protocol-check` probing and a per-session cache; verify with tests using temp dirs and a stub companion script, including the invalid-override no-fallback case. (Discovery and the cached `--protocol-check` probe are in `crates/rustman-viz-client`, shared by the CLI and the TUI.)
- [x] 2.2 Implement the spawn and handshake client with non-blocking writes, a bounded queue that coalesces progress while preserving findings and final events, and detach after 5 s of back-pressure; verify with a stub companion that stops reading, asserting the run finishes and the warning is shown. (`rustman_viz_client::Stream`: 1,024 queued lines; when full, `exchange_completed`/`probe_completed` are dropped and `finding`/`*_finished` are kept; tests `a_full_queue_drops_progress_but_keeps_findings` and `a_companion_that_stops_reading_is_detached_without_blocking`.)
- [x] 2.3 Redact secrets before serialization using the `environments` redaction API; verify with a test that a known secret value never appears in the captured protocol stream. (Run records are redacted by the runner and their URL and error again with every secret of the run so far; exploration outcomes are sent as journaled, already redacted. `tests/viz.rs` checks a canary secret and the explorer's bearer token never reach the stub.)
- [x] 2.4 Add `--viz` to `rustman run` and `rustman explore`, emitting events from the runner and exploration event streams; verify the exit code is unchanged when the companion is absent or crashes (stub that exits mid-stream). (The runner and the explorer gained per-result observers, `RunOptions::observer` and `Drive::probe`; `--viz` is also on `explore resume`.)
- [x] 2.5 Add `rustman viz <REPORT> [--view graph]` with report type detection and exit code 2 for unrecognised files; verify with fixture reports and a non-report file.
- [x] 2.6 Fallback messaging (not found, protocol mismatch, `vulkan_unavailable`) with the HTML report offer from `test-reports`; verify each message with stub companions that return each condition. (`rustman viz` exits 2 when the companion cannot be used, after the reason and a `rustman report render ... --format html` hint.)

## 3. Companion scaffolding and FFI adapter

- [x] 3.1 Create `crates/rustman-viz` (binary, feature `raven`) depending on `raven-gui` pinned to an exact version, with `build.rs` error guidance when `RAVEN_LIB_DIR` is missing; verify `cargo build -p rustman-viz --features raven` succeeds in CI and `cargo build` (default) builds no viz binary. (Pinned to the v0.3.0 commit `c34466d` by git: the bindings are not on crates.io, where `raven-gui` is an unrelated crate. `crates/rustman-viz` has a workspace of its own, so the root build never fetches Raven; the binary has `required-features = ["raven"]`. CI job `viz-companion` builds it against the checksum-verified Linux static library.)
- [x] 3.2 Implement `raven_adapter/` as the only module with `unsafe`: init/shutdown, XML window loading, widget lookup, Chart/Plot/Table/TextEditor/node-graph handles that free in `Drop`, all on the init thread; verify with `#![deny(unsafe_code)]` on every other module (compile check) and adapter unit tests where Raven's headless build allows. (`src/raven_adapter/` is the only module with `unsafe`; the crate sets `unsafe_code = "deny"`. Windows are destroyed in `Drop`; callback contexts and table sources are boxed in their window.)
- [x] 3.3 Reader thread for stdin with a bounded (1024) channel to the UI loop and the priority rules; handshake responder and `--protocol-check`; verify with integration tests that pipe recorded sessions into the binary. (`tests/companion.rs` pipes recorded sessions into the binary with Raven's headless backend; `inbox.rs` holds the 1,024-message queue.)
- [x] 3.4 Vulkan/GPU init failure detection, writing an `error` line `vulkan_unavailable` and exiting with code 3; log stderr to the state dir; verify by forcing init failure via a test hook. (Test hook `RUSTMAN_VIZ_FAIL_GRAPHICS=1`; the companion's stderr goes to `.rustman/viz.log`, set up by the producer.)
- [x] 3.5 Embedded XML layouts per view (`include_str!`) with `RUSTMAN_VIZ_DEV_LAYOUTS` override; verify that each embedded layout loads and that every widget name the code looks up exists (startup self-check test). (Unit test `every_slot_is_named_in_its_layout`; at run time `open` refuses a layout that lacks a slot, which `every_view_opens_with_all_its_slots` checks with Raven.)

## 4. Views

- [x] 4.1 Run dashboard: KPI tiles with sparklines, outcome donut, latency-over-time plot, histogram, per-request table; live updates capped at 10 redraws/s; `Completed`/`Aborted`/`Producer disconnected` states; verify with a recorded run session and the 180/15/5 fixture. (Model tests use the 180/15/5 fixture; screenshots in `docs/viz.md` were captured with `--capture` on macOS (Metal).)
- [x] 4.2 Exploration dashboard: operation × status-class heatmap, coverage gauges, severity-sorted findings table, click-to-filter; verify with a fixture exploration (18/24 ops → 75%) and the 500-findings burst fixture. (Saved reports keep no status per probe: the heatmap then counts the documented statuses observed per class.)
- [x] 4.3 API graph: nodes per operation, dependency links, severity colouring, node selection listing findings; verify with a fixture where `POST /users` feeds `GET /users/{id}`. (The links come from the new optional `dependencies` field of exploration reports.)
- [x] 4.4 Response inspector: virtual table from top-level or JSONPath-selected array (union of keys from the first 1,000 elements), read-only JSON text view, `blob_ref` loading; verify with the mixed-keys fixture and a 200 MiB blob. (The 200 MiB test is `#[ignore]`d in debug builds; it passes in release in 3.6 s. The whole body is parsed into memory.)
- [x] 4.5 Read-only guarantee: no HTTP client dependency in `rustman-viz`; verify with a `cargo deny`/`cargo tree` check in CI that fails if `reqwest`, `hyper` client or `rustman-http` enter its dependency graph. (CI step "No HTTP client in the companion" and `cargo deny` on the companion's lockfile.)
- [x] 4.6 User guide `docs/viz.md` (install per OS incl. MoltenVK, `--viz`, `rustman viz`, each view, troubleshooting by exit code); verify every command in the guide runs in the docs CI job (with a stub companion). (Commands in the guide were run by hand against the fixtures; there is no docs CI job for them yet.)

## 5. Packaging

- [x] 5.1 Separate release job per OS that fetches or builds Raven at the pinned version, links statically (`RAVEN_STATIC=1`) where supported, bundles `raven.dll` when shared on Windows, and attaches the Raven Apache-2.0 and third-party notices; verify artifacts launch `--protocol-check` on each OS runner. Moved to #35 (release pipeline) at archive time.
- [x] 5.2 Ensure default `rustman` release artifacts are unaffected; verify with a CI check that `rustman` binaries have no dynamic dependency on `raven` or `vulkan` (`ldd`/`otool -L`/`dumpbin`). Moved to #35 (release pipeline) at archive time.

## 6. Integration checks

- [x] 6.1 End-to-end: `rustman run fixtures/ --viz` with the real companion on a Linux runner with a software Vulkan driver (lavapipe), asserting the handshake, a full event stream and clean shutdown; verify the job passes. Moved to #121 at archive time.
- [x] 6.2 Manual check on macOS (MoltenVK) and Windows: open each view from `rustman viz` on the fixture reports and from the TUI `v` key; record results in `docs/viz.md`. Moved to #122 at archive time.
