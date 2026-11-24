# Proposal

## Why

The core engine from `bootstrap-core-engine` can be driven from scripts, but humans need an interactive way to browse a workspace, edit requests and read responses. The TUI is Rustman's main face and its selling point against Postman: instant start, keyboard-first, and good-looking in any terminal. Ghostman showed what goes wrong otherwise: a UI that blocks on the network, a JSON-only viewer, a hidden insecure-TLS state, fixed keybindings. This change specifies a TUI that avoids all of these from day one.

## What Changes

- Add `rustman` (no subcommand) / `rustman tui [path]`, which opens a full-screen terminal UI over a workspace.
- Three-region layout: collection tree sidebar, request editor (Params / Headers / Body / Auth / Assertions), response viewer (Body / Headers / Timing / Assertions). Below 120 columns the layout collapses, and it stays usable at 80x24.
- Multiple open requests as tabs, with unsaved-change indicators and save/discard prompts.
- Requests are sent asynchronously and never block the UI. A spinner and elapsed time show progress, and `Esc` / `Ctrl+C` cancels the request in flight.
- Response viewer: a collapsible JSON tree, a JSONPath filter, incremental search, syntax highlighting by content type, a timing waterfall, a latency sparkline from request history, and a diff between two exchanges.
- Environment switcher with secret masking and a per-session reveal toggle.
- Fuzzy command palette (`Ctrl+P`), help overlay (`?`), toasts, mouse support.
- Default keymap, optional vim mode, and user overrides in `keybindings.toml`.
- Themes: `dark` (default), `light` and `high-contrast`. Truecolor with a 256-color fallback; `NO_COLOR` is respected.
- Bodies can be edited in the external `$EDITOR`.
- `v` opens the current exchange or run in the viz companion when available (see `add-raven-visualizer`).
- An extension point for protocol-specific editor panes keyed by request file `kind`. The GraphQL, MCP and webhook changes plug into it.

Out of scope:
- GraphQL, MCP and webhook panes themselves (owned by `add-graphql-client`, `add-mcp-client`, `add-webhook-inspector`).
- OpenAPI exploration UI (owned by `add-openapi-explorer`).
- Request execution, variable resolution, history storage and assertions semantics (owned by `bootstrap-core-engine`; the TUI only renders them).
- Graphical charts beyond terminal glyphs (owned by `add-raven-visualizer`).
- Collaborative/multi-user features.

## Capabilities

### New Capabilities

- `tui`: the interactive terminal front-end — layout, navigation, editing, async sending, response inspection, keymaps, themes and the protocol pane extension point.

### Modified Capabilities

<!-- None: `workspace`, `environments`, `http-requests`, `assertions` and `request-history` are consumed as defined by bootstrap-core-engine, without requirement changes. -->

## Impact

- New crate `rustman-tui`; `rustman-cli` gains the default (no-subcommand) and `tui` entry points.
- New dependencies: `ratatui`, `crossterm`, `tui-textarea` (or equivalent), `syntect`, `nucleo` (fuzzy matching), `similar` (diffs), `serde_json_path`.
- New user config files under the platform config dir: `keybindings.toml`, `tui.toml`.
- No change to the workspace on-disk format.
