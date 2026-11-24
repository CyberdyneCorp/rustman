# Proposal

## Why

A terminal is great for requests but poor for things like a latency histogram across 5,000 probes, a heatmap of operations × status codes, or the dependency graph of an API. Rustman produces exactly that data: runs from `cli-runner` and explorations from `api-exploration`. Users need a graphical view on demand, without turning Rustman into a heavy GUI app or making a GPU a requirement.

Raven (github.com/oscar-soirey/Raven, Apache-2.0) is a light C11 GUI library with Rust bindings (`raven-gui`). It has dashboard widgets (`Chart`: pie, donut, gauge, heatmap, sparkline), `Plot`, `Table`, a JSON-aware text editor, a node graph, and XML-defined windows. It's a good fit for an optional companion.

## What Changes

- New optional binary `rustman-viz`, built only with cargo feature `raven`. It renders Rustman data with Raven and is never required by `rustman`.
- New versioned **viz protocol**: JSON lines over the companion's stdin, opened with a version handshake. `rustman` is the producer; the companion only displays and never sends requests.
- Companion discovery order: `RUSTMAN_VIZ` env var, then `PATH`, then next to the `rustman` executable.
- Four views:
  - **Run dashboard**: KPI tiles, status-class donut, latency plot and histogram, per-request sparklines.
  - **Exploration dashboard**: operation × status heatmap, coverage gauges, findings table by severity.
  - **API graph**: a node graph of operations and their data dependencies.
  - **Large-response inspector**: a table of a JSON array plus a read-only JSON text view.
- Live streaming: dashboards update while a run or exploration is in progress.
- `rustman viz <report.json>` opens a saved run or exploration report in the companion.
- Graceful fallback when the companion or a GPU is unavailable. The TUI and CLI explain why and offer the HTML report from `test-reports` instead.

Out of scope:
- Any request execution, editing or workspace mutation from the GUI (the companion is strictly read-only).
- Making Raven a dependency of `rustman`, `rustman-tui` or any engine crate.
- Web/WASM builds of the companion, and mobile platforms.
- Custom user-authored dashboards (layouts are built in for v1).
- The HTML report format itself (owned by `test-reports`).

## Capabilities

### New Capabilities

- `visual-dashboards`: the optional GUI companion — discovery, the viz protocol, the four views, live updates, the `rustman viz` command, and fallback behaviour.

### Modified Capabilities

<!-- None. `cli-runner`, `api-exploration` and `test-reports` are consumed as-is; the report JSON they already produce is the companion's input. The TUI's `v` hand-off is specified in `tui` (add-terminal-ui). -->

## Impact

- New crate `crates/rustman-viz` (binary) with dependency `raven-gui` (pinned exact version). It needs the Raven C library at build time (`RAVEN_LIB_DIR`, or `RAVEN_STATIC=1`) and a Vulkan loader at run time (MoltenVK on macOS).
- New small crate `rustman-viz-protocol` (pure Rust, serde types only). It's used by `rustman-cli` and `rustman-tui` as producers and by `rustman-viz` as the consumer.
- `rustman-cli` gains the `viz` subcommand. It works whether or not the companion is installed.
- Release pipeline gains separate per-OS `rustman-viz` artifacts. The default `rustman` artifacts are unchanged.
