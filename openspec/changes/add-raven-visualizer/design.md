# Design

## Context

- `cli-runner` and `api-exploration` already produce structured run and exploration reports (JSON) through `test-reports`. They also emit progress events while running.
- config.yaml requires that the default `rustman` binary builds and runs with no C toolchain and no GPU, and that native-library features sit behind a cargo feature and a separate binary.
- Raven facts that shape this design, verified in its repository at v0.2.3:
  - It's a C11 library. The Rust crate `raven-gui` (used as `raven`) links it through `RAVEN_LIB_DIR`, with `RAVEN_STATIC=1` for a static library.
  - It renders with Vulkan, loaded at run time; macOS needs MoltenVK. It has native windows on Win32, X11/Wayland and Cocoa.
  - Widgets: `Chart` kinds Pie, Donut, Gauge, Heatmap and Sparkline; `Plot` for curves over axes; `Table` with virtual rows; a `TextEditor` with JSON highlighting; a node graph.
  - Windows can be written in XML, and `window_from_xml` loads them.
  - The README itself says the public API is still subject to change (pre-1.0).

## Goals / Non-Goals

**Goals:**
- Zero cost to users who never install the companion: no build-time, link-time or run-time trace in `rustman`.
- Keep a crash, hang or GPU failure in the GUI from affecting the TUI, a CI run or an agent session.
- Keep the unsafe FFI surface small, audited and in one place.
- Absorb Raven API churn in one adapter module.

**Non-Goals:**
- Feature parity with a BI tool. The views are fixed and purpose-built.
- Two-way interaction that changes Rustman state. Clicking in the GUI may filter or drill down locally, but never triggers requests.

## Decisions

### D1. Out-of-process companion, not linked into `rustman`
`rustman-viz` is a separate executable. `rustman` spawns it and writes protocol messages to its stdin.
- Keeps the C/Vulkan dependency out of the main binary (hard constraint).
- Isolates faults: a Vulkan driver crash or a GUI hang kills only the companion. `rustman` sees a broken pipe and shows a toast.
- Raven owns its main loop (`Rwait_frame`) and its render thread. Embedding it in a tokio/ratatui process would fight over the main thread on macOS (Cocoa requires the UI on the main thread).
- Users of a headless box, CI or an agent never pay for it.

*Rejected:* linking Raven under a feature flag in `rustman` itself. That gives two binaries with different crash profiles under one name, puts main-thread contention with the TUI on macOS, and makes `cargo install rustman --features raven` fail for users without the C library.
*Rejected:* a web dashboard served on localhost. That needs a browser, an HTTP server and a JS bundle, and the HTML report from `test-reports` already covers "no GPU".

### D2. Viz protocol: JSON lines over stdin, versioned handshake
- Transport: the child's stdin, as UTF-8 JSON, one message per line, with lines up to 16 MiB. Bulk data larger than that (huge responses) goes through a temp file referenced by path (`{"type":"blob_ref","path":...}`).
- The child's stdout carries only the handshake reply and `ack`/`error` lines. Its stderr goes to a log file in the Rustman state dir.
- Handshake: producer sends `{"type":"hello","protocol":"rustman-viz/1","producer":"rustman <ver>"}`. Companion replies `{"type":"hello_ack","protocol":"rustman-viz/1","viewer":"rustman-viz <ver>","views":[...]}` or `{"type":"hello_reject","supported":["rustman-viz/1"]}`.
- Versioning: the major version bumps on breaking changes. Minor additions are new optional fields or message types, and unknown ones are ignored.
- Messages: `open_view`, `run_started`, `exchange_completed`, `run_finished`, `exploration_started`, `probe_completed`, `finding`, `exploration_finished`, `load_report`, `inspect_response`, `close`.
- Why stdin and not a socket: there's no port to allocate or secure, it works on all three OSes, and the lifetime is tied to the parent (EOF means the producer is gone).

*Rejected:* gRPC or a local TCP socket (a port and auth surface, more dependencies); shared memory (complex, platform-specific).

### D3. Discovery order
1. `RUSTMAN_VIZ` (absolute path to the executable; if set but invalid, report it and stop, without falling through).
2. `rustman-viz` on `PATH`.
3. `rustman-viz[.exe]` in the same directory as the running `rustman` executable.

The first hit is spawned with `--protocol-check` (it prints its supported versions and exits) before real use. The result is cached per session.

The producer side (discovery, the probe, the handshake and the live stream) lives in `crates/rustman-viz-client`, which both `rustman-cli` and `rustman-tui` use. It depends only on the protocol crate. Callers redact before sending. The live stream writes from its own thread through a bounded queue of 1,024 lines. When the queue is full, progress messages are dropped and `finding` and `*_finished` are kept. After 5 s of back-pressure the producer detaches.

### D4. FFI containment and the adapter layer
- All `unsafe` calls into `raven` live in `crates/rustman-viz/src/raven_adapter/`. The rest of the crate talks to a safe internal trait (`Surface`, `ChartHandle`, `TableHandle`, `GraphHandle`).
- Inside the adapter: `#![deny(unsafe_op_in_unsafe_fn)]`, a `// SAFETY:` comment on every block, a handle type that frees its widget in `Drop`, and all Raven calls on the thread that called `Rinit`.
- Protocol decoding runs on a separate reader thread and hands typed updates to the UI thread through a bounded channel (capacity 1024; the newest progress update wins when it's full, while `finding` and `*_finished` are never dropped).
- `raven-gui` is pinned to an exact version (`=0.2.3` at writing). Upgrades are explicit PRs that touch only the adapter. CI builds against the pinned version.

### D5. Window layouts as embedded XML
Each view's skeleton is an XML file compiled into the binary (`include_str!`), loaded with `window_from_xml`, and its widgets looked up by name. Data is pushed through the adapter. With `RUSTMAN_VIZ_DEV_LAYOUTS=<dir>`, layouts are loaded from disk instead, so Raven's hot reload can be used while designing. That's a development aid, not a user feature.

### D6. View content mapping
| View | Raven widgets | Data source |
|------|---------------|-------------|
| Run dashboard | `Chart` Sparkline KPI tiles (requests, pass %, p50, p95, errors), `Chart` Donut (2xx/3xx/4xx/5xx/transport errors), `Plot` latency over time, `Plot` histogram (bucketed), `Table` per-request rows with an embedded sparkline | run events / run report |
| Exploration dashboard | `Chart` Heatmap (operations × status classes), `Chart` Gauge (operation coverage %, status-code coverage %), `Table` findings sorted by severity, then operation | exploration events / report |
| API graph | node graph: one node per operation, links for data dependencies (a capture from A feeds a parameter of B), colour by worst finding severity | exploration report dependency graph |
| Response inspector | `Table` (virtual rows) for a top-level or JSONPath-selected array, columns from the union of keys in the first 1,000 elements; read-only `TextEditor` with JSON highlighting | `inspect_response` / blob_ref |

### D7. Packaging
- `rustman` release artifacts are unchanged.
- `rustman-viz` artifacts per OS are built in a separate CI job. It downloads the matching prebuilt Raven release, links statically (`RAVEN_STATIC=1`) where the platform allows, and bundles `raven.dll` next to the executable on Windows if it's built shared. On macOS the release notes say to install MoltenVK (`brew install molten-vk`).
- `cargo install rustman-viz` documents the `RAVEN_LIB_DIR` requirement. `build.rs` fails with an actionable message when the library isn't found.
- Apache-2.0 notice for Raven (and its `third_party` notices) is shipped with the `rustman-viz` artifacts.

### D8. Fallback behaviour
| Condition | Detected by | User-facing result |
|-----------|-------------|--------------------|
| Companion not found | discovery | toast/CLI message `Viz companion not found` + offer HTML report |
| Protocol mismatch | `--protocol-check` / `hello_reject` | message naming both versions + upgrade hint |
| No GPU / Vulkan init fails | companion exits with code 3 and an `error` line `vulkan_unavailable` | message `Graphics unavailable (Vulkan)` + offer HTML report |
| Companion crashes mid-session | broken pipe / non-zero exit | toast `Viz companion exited`; producer keeps running unaffected |

## Risks / Trade-offs

- [Raven is pre-1.0 and changes daily] → Exact version pin, a single adapter module, and CI that builds `rustman-viz` on every PR so breakage shows up immediately. Because the companion is optional, a broken Raven upgrade never blocks a `rustman` release.
- [Vulkan unavailable on VMs, remote desktops and some CI runners] → The documented fallback to the HTML report, and the companion's exit code 3 is distinct from a crash. The companion's CI only builds and runs protocol/unit tests with the Raven `headless` build where possible; rendering is checked manually.
- [macOS needs MoltenVK] → Detected at startup with a specific error message and install hint.
- [Large reports flood the pipe] → Line cap, `blob_ref` for bulk, coalesced progress updates, and a bounded channel with priority for findings and final events.
- [FFI memory unsafety] → Contained per D4. Run the adapter tests under Miri where possible, and run the protocol fuzz target (`cargo fuzz`) on the decoder, which is pure Rust.

## Migration Plan

New optional component, nothing to migrate. Ships after `add-openapi-explorer`, because the exploration dashboard and API graph consume its reports. The run dashboard alone could ship earlier.

## Open Questions

- Will Raven publish prebuilt static libraries for all three OSes on each release, or should our CI build Raven from source at the pinned commit? This only affects the packaging job, not the specs.
