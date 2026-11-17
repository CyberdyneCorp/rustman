# Design

## Context

`bootstrap-core-engine` provides the engine crates (`rustman-core`, `rustman-http`) with an async API: load a workspace, resolve variables against the active environment, execute a request file into an exchange, and read request history. The TUI is a thin adapter over that API (see config.yaml: no UI code in engine crates). See proposal.md for why.

## Goals / Non-Goals

**Goals:**
- Keep the UI responsive while anything is happening: render ≤16 ms per frame, never `await` network or disk I/O on the render loop.
- Make it usable on a plain 80x24 terminal with no mouse and no colors, and attractive on a modern truecolor terminal.
- Make every action reachable from the keyboard, discoverable through the palette, and remappable.
- Leave a clean seam for protocol panes added by later changes.

**Non-Goals:**
- Pixel graphics (sixel/kitty images). Charts stay in glyphs here; rich charts go to the viz companion.
- A plugin system for third-party panes. The extension point is compile-time and internal.

## Decisions

### D1. ratatui + crossterm
ratatui is the de-facto Rust TUI library: immediate-mode, a rich widget set, and an active project. crossterm works on Windows, macOS and Linux without terminfo quirks.
*Rejected:* `cursive` (retained-mode, weaker layout control), `termion` (no Windows support).

### D2. Elm-style architecture: `State` + `Action` + `update` + `view`
Input events and engine events are both turned into `Action`s on a single `tokio::mpsc` channel. `update(state, action)` is pure and synchronous. Side effects (send a request, open `$EDITOR`, save a file) are returned as `Effect`s that a task runner executes, and their completions come back as `Action`s. Rendering is `view(&state, frame)` only.
This makes the UI testable with ratatui's `TestBackend` and snapshot tests (`insta`). It also makes "never blocks on network" structural rather than a matter of discipline.
*Rejected:* widgets owning state and calling the engine directly. That is the source of Ghostman's freeze, where a send was awaited inside the key handler.

### D3. Cancellation via `CancellationToken`
Each in-flight send owns a `tokio_util::sync::CancellationToken`. `Esc` / `Ctrl+C` cancels the token for the focused tab. The engine future is `select!`ed against it, so the connection is dropped and the exchange is recorded as `cancelled`. `Ctrl+C` with nothing in flight never quits; quitting is `Ctrl+Q`, so a cancel keystroke can't kill the app.

### D4. Keymap as data
Actions have stable string ids (`request.send`, `palette.open`, …). Keymaps are layered: built-in default → optional `vim` preset → user `keybindings.toml`. Each layer maps `context + key chord → action id`, where the context is `global`, `sidebar`, `editor`, `response`, etc. Conflicts are resolved in that order: the user always wins. Unknown action ids are reported at startup as a toast and ignored. The palette lists every action with its current binding, so it doubles as live documentation.

### D5. Responsive layout breakpoints
- ≥120 cols: sidebar (28 cols, resizable 20–50) | editor | response (split 50/50, or stacked if the user toggles).
- 80–119 cols: sidebar collapsed to a toggleable overlay (`Ctrl+B`); editor and response stacked vertically.
- <80 cols or <24 rows: single pane with a pane switcher bar. A notice says the terminal is below the recommended size.

### D6. JSON viewer
The body is parsed once off the render thread into an indexed tree of nodes with collapsed flags. Only visible rows are materialized (a virtual list), so a 50 MB response scrolls smoothly. Filtering uses JSONPath (RFC 9535) via `serde_json_path`. jq syntax is a non-goal for v1, because it would mean a large dependency (`jaq`) or shelling out. The filter box accepts JSONPath, and the docs say so.
Non-JSON bodies use `syntect` highlighting chosen by `Content-Type` (HTML, XML, YAML, plain). Binary bodies show a hex view plus size, and offer "save to file".

### D7. Theming and colour capability detection
Themes are TOML palettes of semantic tokens (`accent`, `ok`, `warn`, `error`, `muted`, `selection`, …), never raw colours in widget code. Capability detection:
- `NO_COLOR` set (any value) → monochrome. Status uses symbols or text (`✔ 200`, `✖ 500`) and reverse-video selection.
- `COLORTERM` = `truecolor` or `24bit` → RGB.
- Otherwise → the nearest xterm-256 colour.
Users can force a theme with `--theme` or in `tui.toml`.

### D8. Protocol pane extension point
A `RequestPane` trait, registered by `kind` (`http` ships here; `graphql`, `mcp` and `webhook` register theirs in later changes), provides: the editor tab list, how to render each tab, how to turn edits into a request file, and the response tabs. An unknown `kind` renders a read-only raw view of the request file. This lets later changes add panes without editing the core layout code.

### D9. External editor
`Ctrl+E` writes the current body to a temp file with an extension derived from the `Content-Type`. It suspends the TUI (leaves the alternate screen, restores cooked mode), runs `$VISUAL`, else `$EDITOR`, else `vi` (or `notepad` on Windows), waits for it, then restores the TUI and reloads the body.

### D10. Viz hand-off
`v` asks `add-raven-visualizer`'s discovery for a companion. If one is found, the TUI streams the current exchange or run over the viz protocol. Otherwise it shows a toast with the fallback. The TUI depends only on the protocol types (a small `rustman-viz-protocol` module), never on Raven.

## Risks / Trade-offs

- [Terminal diversity: Windows conhost, tmux, screen mis-render box drawing or colours] → Box-drawing set selectable (`unicode` | `ascii`), CI snapshot tests at 80x24 and 200x50 in both sets, manual check matrix in tasks.
- [Large responses blow memory] → Bodies above 10 MiB (configurable) are kept on disk and viewed lazily. The tree view is disabled above 50 MiB, with a raw paged view instead.
- [Keymap conflicts with terminal or multiplexer keys (`Ctrl+B` in tmux, `Ctrl+S` XOFF)] → Raw mode disables XON/XOFF. Defaults avoid `Ctrl+S` for send (send = `Ctrl+Enter`, with `Ctrl+R` as a fallback because many terminals can't encode `Ctrl+Enter`). Every default is remappable.
- [Vim mode scope creep] → Vim mode covers navigation and modal editing in text areas only (normal/insert/visual with common motions). It is not a full vim emulation.

## Migration Plan

New capability, nothing to migrate. Ships behind no flag. `rustman` with no args opens the TUI once this change lands. Before that it prints help.

## Open Questions

- Should the sidebar order be alphabetical or a user-defined order stored in the workspace? This affects `workspace` (core), not the TUI contract. Default here: honour whatever order `workspace` reports.
