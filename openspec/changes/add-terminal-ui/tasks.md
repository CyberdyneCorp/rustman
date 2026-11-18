# Tasks

## 1. Crate scaffolding and app loop

- [x] 1.1 Create `crates/rustman-tui` with `ratatui`, `crossterm`, `tokio`, `tokio-util` dependencies and wire `rustman` (no subcommand) and `rustman tui [PATH]` in `rustman-cli`; verify `cargo run -- tui --help` prints usage.
- [x] 1.2 Implement the `State`/`Action`/`update`/`view` loop with one `mpsc` action channel and an effect runner task; verify with a unit test that `update` is synchronous and returns effects instead of performing I/O.
- [x] 1.3 Terminal setup/teardown guard plus panic hook that restores cooked mode and the main screen; verify with an integration test that triggers a panic under a pty (`expectrl`/`portable-pty`) and asserts the terminal is restored.
- [x] 1.4 Workspace discovery (walk ancestors for `rustman.toml`) and the "Create workspace / Scratch session" prompt; verify with tests for subdirectory discovery and the no-workspace prompt.

## 2. Layout, focus and theming

- [x] 2.1 Implement the three breakpoints (≥120, 80–119, <80 cols or <24 rows), sidebar resize (20–50) and `Ctrl+B` overlay; verify with `insta` snapshot tests at 160x45, 80x24 and 60x20.
- [x] 2.2 Status bar (workspace, environment, TLS badge, in-flight count, vim mode, `SECRETS VISIBLE`); verify with snapshot tests for each badge state.
- [x] 2.3 Theme engine with semantic tokens and the `dark`, `light`, `high-contrast` themes; colour detection (`COLORTERM`, xterm-256 fallback, `NO_COLOR`); `--ascii` glyph set; verify with unit tests for colour mapping and snapshot tests in `NO_COLOR` and ASCII modes.
- [x] 2.4 Focus ring with `Tab`/`Shift+Tab` and mouse focus/click/scroll/drag-resize, with a `mouse = false` option; verify with event-driven tests on `TestBackend`.
- [x] 2.5 Toast system (3-deep stack, timeouts per severity) and notification log; verify with tests using a mocked clock.

## 3. Keymap, palette and help

- [x] 3.1 Action registry with stable ids; layered keymaps (default → vim → `keybindings.toml`) with per-context chords; startup validation that toasts unknown ids or bad chords; verify with unit tests covering override precedence and invalid entries.
- [x] 3.2 Fuzzy command palette (`nucleo`; its `nucleo-matcher` crate, without the worker pool) over actions, request files and environments, with recency ranking; verify with tests that `swenv` ranks "Switch environment" first and that bindings are displayed.
- [x] 3.3 Help overlay generated from the effective keymap; verify a snapshot shows a user-rebound action with its new chord.
- [x] 3.4 Vim mode: navigation in lists/trees and modal editing in text areas (normal/insert/visual, listed motions); verify with key-sequence tests and the status-bar mode indicator.
- [x] 3.5 Document the default keymap, the vim preset and the `keybindings.toml` format in `docs/tui.md`; verify the doc table is generated from the registry by a test that fails on drift.

## 4. Sidebar, tabs and editing

- [ ] 4.1 Collection tree from the `workspace` API with kind/method badges, `/` fuzzy filter, `n`/`r`/`d` with delete confirmation; verify with tests against a fixture workspace.
- [ ] 4.2 Tabs: open/focus-existing, `Ctrl+T`, `Ctrl+W`, `Alt+1..9`, `Ctrl+PgUp/PgDn`, independent per-tab state; verify with a test that keeps tab A in flight while editing tab B.
- [ ] 4.3 Dirty tracking, `Ctrl+S` save, Save/Discard/Cancel prompts on close and quit, file-watch reload or conflict warning (`notify` crate); verify with tests for each path, including an external modification.
- [ ] 4.4 `RequestPane` trait and registry keyed by `kind`; read-only fallback for unknown kinds; verify with a test opening a `kind: grpc` fixture.
- [ ] 4.5 HTTP pane: method and URL, Params/Headers tables with enable toggles and URL↔Params sync, Body modes (none, raw, form, multipart), Auth, Assertions editors; verify with unit tests for sync and an end-to-end test that a disabled header is not sent (local `wiremock` server).
- [ ] 4.6 Variable highlighting and resolution tooltip with layer and secret masking; verify with snapshot tests for resolved, unresolved and secret variables.
- [ ] 4.7 External editor flow (`$VISUAL` → `$EDITOR` → `vi`/`notepad`) with suspend/resume and temp-file cleanup; verify with a test that uses a fake editor script that rewrites the file.

## 5. Sending and cancellation

- [ ] 5.1 Send effect over the engine API with a per-tab `CancellationToken`, spinner and live elapsed time; verify with a `wiremock` 10 s delay test asserting keypresses render within 50 ms while the request is pending.
- [ ] 5.2 `Esc`/`Ctrl+C` cancel that records a `cancelled` exchange and closes the connection; `Ctrl+C` idle shows the quit hint; verify with tests asserting connection drop on the mock server and the idle toast.

## 6. Response viewer

- [ ] 6.1 Summary line (status class colours, time, size, assertion tally, transport error categories, insecure marker); verify with snapshot tests for 2xx/4xx/5xx/DNS failure.
- [ ] 6.2 Headers tab (received order, duplicates), Assertions tab (expected/actual), captured variables (masked); verify with fixture exchanges.
- [ ] 6.3 JSON tree viewer: off-thread parse into an indexed tree, virtual list, toggle/collapse-all bindings, counts on collapsed arrays, raw toggle, copy via clipboard or OSC 52; verify with unit tests and a benchmark that scrolling a 50 MiB body stays under 16 ms per frame.
- [ ] 6.4 JSONPath filter (`serde_json_path`) with inline errors, and incremental search with `n`/`N`; verify with tests for valid, invalid and empty-match filters.
- [ ] 6.5 `syntect` highlighting by content type, hex view and "Save body…" for binary; spooling above `max_inline_body`, paged raw view above 50 MiB; verify with fixture responses (XML, PNG, 80 MiB JSON).
- [ ] 6.6 Timing waterfall from the exchange's phase timings, with `—` for skipped phases; verify with snapshot tests for fresh and reused connections.
- [ ] 6.7 Latency sparkline of the last 30 exchanges from `request-history` with min/median/p95/max; verify with a fixture history and the `not enough history` case.
- [ ] 6.8 Exchange diff (`D`): picker, structural JSON diff ignoring key order, line diff otherwise (`similar`); verify with the `{"a":1,"b":2}` vs `{"b":3,"a":1}` case reporting only `$.b`.

## 7. Environments and secrets

- [ ] 7.1 Environment switcher (`Alt+E`) listing variables with layer; activating updates the status bar and next send; verify with a test that the next exchange uses the new environment's values. (Already done in group 3: the palette's environment switcher sets the active environment shown in the status bar, in memory only; see `commands.rs`. Remaining: the variable/layer listing and wiring into the next send.)
- [ ] 7.2 Secret masking in every surface and session-only reveal (`Alt+R`) with the `SECRETS VISIBLE` badge; verify with a test that scans the rendered buffer for a known secret value and finds it only while revealed.
- [ ] 7.3 Persistent `TLS: INSECURE` badge; verify with a snapshot test using an environment that disables verification.

## 8. Viz hand-off

- [ ] 8.1 `v` action that calls viz discovery and streams the current exchange or run, with a toast fallback offering HTML export; verify with a test that uses a stub companion binary echoing the protocol handshake, plus a test with none found.

## 9. Integration checks

- [ ] 9.1 Cross-platform smoke test in CI (Linux, macOS, Windows): launch under a pty at 80x24, open a fixture request, send it to a local mock, cancel a slow one, quit; verify the job passes on all three OSes.
- [ ] 9.2 Manual check matrix recorded in `docs/tui.md`: Windows Terminal, conhost, iTerm2, Alacritty, kitty, tmux and GNOME Terminal, in truecolor, 256-colour and `NO_COLOR` modes.
