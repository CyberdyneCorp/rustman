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
- `NO_COLOR` set to a non-empty value → monochrome. Status uses symbols or text (`✔ 200`, `✖ 500`) and reverse-video selection.
- `COLORTERM` = `truecolor` or `24bit` → RGB.
- Otherwise → the nearest xterm-256 colour.
Users can force a theme with `--theme` or in `tui.toml`.

### D8. Protocol pane extension point
A `RequestPane` trait, registered by `kind` (`http` ships here; `graphql`, `mcp` and `webhook` register theirs in later changes), provides: the editor tab list, how to render each tab, how to turn edits into a request file, and the response tabs. An unknown `kind` renders a read-only raw view of the request file. This lets later changes add panes without editing the core layout code.

### D9. External editor
`Ctrl+E` writes the current body to a temp file with an extension derived from the `Content-Type`. It suspends the TUI (leaves the alternate screen, restores cooked mode), runs `$VISUAL`, else `$EDITOR`, else `vi` (or `notepad` on Windows), waits for it, then restores the TUI and reloads the body. The variable is split POSIX-style on Unix; on Windows a value naming an existing file is the program as is, and anything else is split with the Windows command-line rules (backslashes literal unless before a quote), so `C:\Windows\System32\notepad.exe` survives. When the configured editor cannot be started, `vi`/`notepad` opens instead with a warning toast.

### D10. Viz hand-off
`v` asks `add-raven-visualizer`'s discovery for a companion. If one is found, the TUI streams the current exchange or run over the viz protocol. Otherwise it shows a toast with the fallback. The TUI depends only on the protocol types (a small `rustman-viz-protocol` module), never on Raven.

### D11. Disabled rows live in the request file; files are watched with `notify`
The per-row enable toggle of Params, Headers and form fields is stored in the request file as `enabled: false` on that row (`workspace` delta in this change), so a toggled row survives saving and is skipped by every sender (the engine filters it when resolving). Keeping toggles in the TUI only was rejected: saving would silently drop the row.
Unknown request kinds load as opaque requests through `KindRegistry::with_opaque_fallback` in the TUI only, so a `kind: grpc` file opens read-only instead of failing the whole workspace; the CLI keeps rejecting unknown kinds.
Changes on disk are followed with the `notify` crate (inotify, FSEvents, ReadDirectoryChangesW; no native libraries). Its events become `FilesChanged` actions; re-reading a file is an effect, and the comparison against the tab's saved state happens in `update`, so the echo of our own save is a no-op. Polling files from the loop was rejected (disk I/O on the render loop).

### D12. Sends run on their own thread through the engine's runner; the viewer keeps only redacted data
`request.send` returns an effect carrying the edited request, a copy of the workspace and the tab's `CancellationToken`. The effect runner executes it on a blocking thread with a current-thread runtime of its own and `rustman_http::runner::Runner::send`, so resolution, safety settings, cookies, captures, history and redaction are exactly the CLI's. Cancelling drops the request future (and, with the job, its runtime and connection pool), which closes the connection; the tab records the exchange as cancelled (`skip_reason: interrupted`) with its elapsed time. History keeps no entry for it, as for an interrupted `rustman run`. A completion carries the tab's send number, so a late answer to a cancelled or superseded send is dropped.
The finished exchange is prepared on that same thread: the body is copied through the exchange's `Redactor` (into a spool file of the viewer's own when the engine spilled it above `max_body_bytes`, which is the spec's `max_inline_body`), JSON is parsed into a flat node list with subtree ends (folding skips a subtree in one step; drawing reads only the visible rows), other text is highlighted with `syntect` (pure-Rust `fancy-regex`, scopes mapped to theme tokens), and the latencies of the last 30 exchanges are read from history. JSONPath filters, the pages of bodies above 50 MiB, the diff picker's history and saving a body are effects too.
Copy (`y`) uses the platform's clipboard tool (`pbcopy`, `clip`, `wl-copy`, `xclip`, `xsel`) and falls back to OSC 52 written by the loop. *Rejected:* `arboard` (X11/Wayland libraries on Linux), sending on the UI runtime (a large body's decoding would share the render thread).

### D13. Reveal reads secrets through an effect; viz discovery and the HTML fallback live in the TUI for now
Secrets stay unread on the render loop. `Alt+R` returns an effect that reads the active environment's secrets through the engine's `SecretStore`; the values are kept in memory as `Secret` (whose `Debug` is redacted), only while the badge is on and only for the environment they were read for, and are never written anywhere. Exchanges are not un-redacted: the viewer, reports and the viz stream only ever hold the engine's redacted data, so a secret echoed in a response shows the engine's `«redacted:<name>»` marker (as in history and copies) rather than the TUI's `••••••••`, which is kept for the TUI's own surfaces (editors, tooltips, the switcher, captured values). *Rejected:* keeping unredacted exchanges for the reveal (a second copy of every secret in memory and a way for a secret to reach a saved body).
The TLS badge is recomputed after every `update` from `rustman.toml`'s `verify_tls` and the active tab's request settings. Environment files have no TLS setting (`deny_unknown_fields` in `rustman-core`), so the spec's "Insecure TLS indicator" names the workspace default every environment sends with, plus the request's own setting; a per-environment setting would be a `workspace` data-model change of its own.
`add-raven-visualizer`'s producer side (task group 2: discovery, spawn, handshake) is not built yet, so the TUI carries a minimal client in `rustman_tui::viz`: discovery in the spec's order (read once per session), spawn, `hello` with a 10 s answer limit, then `open_view`, `inspect_response` (or its `blob_ref`) and `close`, all built from the protocol crate's types. It moves to the shared producer module when that group lands. The "Export HTML report" fallback writes the current exchange as one self-contained, escaped HTML page from the redacted record, until `test-reports` provides the HTML run report. *Rejected:* waiting for both changes (`v` would stay a "not available" toast in v0.2).

### D14. Smoke test: a real pty on every OS, plus the real event loop in memory
The smoke test re-runs its own test binary as the child under `portable-pty` (a Unix pty, or ConPTY on Windows), so `rustman_tui::run` runs with the real terminal guard, input reader and effect runner, and reads the screen back through the `vt100` emulator (matching raw output is unreliable because ratatui redraws only changed cells). crossterm enables VT processing on Windows 10+ and writes escape sequences there too, so the restored main screen is observable through ConPTY; because ConPTY re-renders output, the raw `?1049h/l` bytes are asserted on Unix only. ConPTY is opened with `PSEUDOCONSOLE_INHERIT_CURSOR`, so the test answers its cursor position query. The same script also drives `run_app` with an in-memory terminal on every OS, which separates a runner's console problem from a TUI regression.

## Risks / Trade-offs

- [Terminal diversity: Windows conhost, tmux, screen mis-render box drawing or colours] → Box-drawing set selectable (`unicode` | `ascii`), CI snapshot tests at 80x24 and 200x50 in both sets, manual check matrix in tasks.
- [Large responses blow memory] → Bodies above 10 MiB (configurable) are kept on disk and viewed lazily. The tree view is disabled above 50 MiB, with a raw paged view instead.
- [Keymap conflicts with terminal or multiplexer keys (`Ctrl+B` in tmux, `Ctrl+S` XOFF)] → Raw mode disables XON/XOFF. Defaults avoid `Ctrl+S` for send (send = `Ctrl+Enter`, with `Ctrl+R` as a fallback because many terminals can't encode `Ctrl+Enter`). Every default is remappable.
- [Vim mode scope creep] → Vim mode covers navigation and modal editing in text areas only (normal/insert/visual with common motions). It is not a full vim emulation.

## Migration Plan

New capability, nothing to migrate. Ships behind no flag. `rustman` with no args opens the TUI once this change lands. Before that it prints help.

## Open Questions

- Should the sidebar order be alphabetical or a user-defined order stored in the workspace? This affects `workspace` (core), not the TUI contract. Default here: honour whatever order `workspace` reports.
