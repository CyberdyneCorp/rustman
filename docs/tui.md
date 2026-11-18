# Terminal UI

`rustman` with no subcommand, or `rustman tui [PATH]`, opens the full-screen terminal UI. The TUI is being built in stages ([tasks](../openspec/changes/add-terminal-ui/tasks.md)); this page describes what works today.

## Starting

```text
rustman              # open the TUI from the current directory
rustman tui          # the same
rustman tui api/     # start the search in api/ instead
rustman tui --theme light --ascii
rustman tui --vim    # vim keys for this run
```

The TUI looks for `rustman.toml` in PATH (default: the current directory) and then in each parent directory, and opens the first workspace it finds. Running it from `/proj/api/users` opens the workspace rooted at `/proj`.

If there is no `rustman.toml` anywhere above PATH, a prompt offers two choices:

- **Create workspace here** creates a workspace in PATH, exactly like `rustman init PATH`, and opens it.
- **Scratch session** opens an empty session that is not backed by any file.

Use `↑`/`↓` (or `Tab`, `j`/`k`) and `Enter`, or press `c` or `s` directly. If the workspace can't be created, the error is shown in the prompt and you can choose again.

`rustman` with no subcommand only opens the TUI when stdin and stdout are a terminal; in a pipe or a script it prints the help instead. `rustman tui` without a terminal fails with exit code 2 and `error: the terminal UI needs an interactive terminal (stdin and stdout must be a TTY)`.

## Quitting and terminal restore

`Ctrl+Q` quits. `Ctrl+C` never quits (it is reserved for cancelling a request in flight).

The TUI runs in raw mode on the terminal's alternate screen. On exit, and also if Rustman panics, the terminal is restored first (cooked mode, main screen, visible cursor), so any panic message lands on your normal screen and the shell keeps working.

The terminal is also restored when the process is told to stop: `SIGTERM`, `SIGHUP` (the terminal window was closed) or `SIGINT` from `kill` on Linux and macOS, and closing the console, logging off, shutting down or `Ctrl+Break` on Windows. Rustman then exits with status 128 plus the signal number (143, 129, 130; 130 on Windows). If background work (loading or creating a workspace) panics, or reading the terminal fails, the TUI also closes cleanly and prints the error, exit status 1.

## Layout

The shell adapts to the terminal size:

| Size | Layout |
|---|---|
| 120 columns or more | Sidebar (28 columns by default), request editor and response viewer side by side. `Ctrl+B` hides or shows the sidebar. |
| 80–119 columns | Request editor above the response viewer. The sidebar is hidden; `Ctrl+B` shows it as an overlay over the left edge (and again, or `Esc`, closes it). |
| Under 80 columns or under 24 rows | One pane at a time, with a pane switcher bar (`Sidebar`, `Request`, `Response`) on top and the notice `Smaller than the recommended minimum of 80x24` at the bottom. `Tab` moves between panes; `Ctrl+B` jumps to the sidebar and back. |

Resizing the terminal re-lays out the screen without losing focus, sidebar width or anything else. The sidebar is 20 to 50 columns wide: drag its right border with the mouse, or use `Alt+Right` / `Alt+Left`.

The sidebar lists the workspace's request files (read-only for now; `↑`/`↓`, `Home`/`End` and `PageUp`/`PageDown` move the selection). The request editor's method, URL and tabs and the response viewer are placeholders until editing and sending arrive; the body is a scratch text area (see [Vim mode](#vim-mode)).

### Focus

`Tab` and `Shift+Tab` move the focus around a ring: sidebar, method, URL, editor tabs, editor content, response viewer, and back to the sidebar. A hidden sidebar is skipped. The focused pane has a highlighted border and a `›` before its title.

### Status bar

The bottom line shows, from the left: the workspace name (or `Scratch session`), the active environment (`env: NAME` or `no environment`), `TLS verified` or a red `TLS: INSECURE` badge when `verify_tls = false`, and how many requests are in flight. On the right: the vim mode (`NORMAL`, `INSERT`, `VISUAL`) when vim keys are on, a `SECRETS VISIBLE` badge in the warning colour while secrets are revealed, and the quit hint.

### Toasts and the notification log

Messages appear as toasts in the top-right corner, newest on top, at most three at a time. Info and success toasts disappear after 3 seconds; warnings and errors stay until dismissed or for 10 seconds. `Esc` dismisses the newest toast, and clicking a toast dismisses it. Every toast is also kept in the notification log (the last 500), which **Show notification log** in the command palette opens; `Esc` closes it.

Warnings found while loading the workspace, a `tui.toml` that can't be read, and bad entries in `keybindings.toml` are shown as warning toasts.

## Mouse

When the terminal reports mouse events, clicking focuses a pane, an editor field or a pane switcher label; the wheel scrolls the pane under the pointer; and dragging the sidebar's right border resizes it. Set `mouse = false` in `tui.toml` to leave the mouse to the terminal, so its own text selection works; the TUI then doesn't capture mouse events at all.

## Themes, colours and ASCII

Three themes ship: `dark` (the default), `light` and `high-contrast`. Pick one with `--theme NAME` or `theme = "NAME"` in `tui.toml`; the flag wins. Themes are palettes of semantic colours (`text`, `muted`, `accent`, `border`, `focus`, `selection`, `ok`, `info`, `warn`, `error`, `background`), so every part of the UI changes together.

The colour depth is detected from the environment:

- `NO_COLOR` set to a non-empty value: no colours at all. Focus, selection and badges use bold, dim and reverse video, and status uses symbols (`✔`, `✖`).
- `COLORTERM=truecolor` or `COLORTERM=24bit`: 24-bit colour.
- Anything else: each theme colour is mapped to its nearest xterm-256 colour.

`--ascii` (or `borders = "ascii"` in `tui.toml`) replaces box drawing and other non-ASCII symbols with ASCII: borders use `+`, `-` and `|`, the pointer is `>`, and symbols such as `✖` become `x`. Use it on terminals or fonts that mis-render box drawing.

## Settings: `tui.toml`

The TUI reads `tui.toml` from the Rustman config directory: `$XDG_CONFIG_HOME/rustman/`, else `~/.config/rustman/` on Linux and macOS, and `%APPDATA%\rustman\` on Windows. Every key is optional:

```toml
theme = "dark"       # dark | light | high-contrast
borders = "unicode"  # unicode | ascii
mouse = true         # false leaves the mouse to the terminal
keymap = "default"   # default | vim (same as --vim)
```

Unknown keys are ignored. An invalid file is ignored as a whole, with a warning toast naming the problem.

## Keys

Every key runs an *action* with a stable id such as `request.send`. Bindings belong to a context: `global`, `sidebar`, `editor` (method, URL, editor tabs and body) or `response`. A key is looked up in the focused context first, then in `global`.

While a text input has the focus (the request body; the URL once it becomes editable), typed characters are text: `?`, `v` and other plain keys type themselves instead of running their action. Keys with `Ctrl` or `Alt`, function keys, `Tab` and `Esc` still work.

`Ctrl+C` never quits. With a request in flight it cancels it; otherwise it shows `Press Ctrl+Q to quit`. Actions that later stages add (sending, saving, tabs, the external editor, viz) are already bound and listed; until they arrive they show a `… is not available yet` toast.

### Command palette

`Ctrl+P` opens a fuzzy finder over every action (with its current keys on the right), every request file and every environment. Type to filter: the letters only have to appear in order, so `swenv` finds **Switch environment**. `↑`/`↓` (or `Ctrl+P`/`Ctrl+N`) choose, `Enter` runs, `Esc` closes. With nothing typed, the entries you used most recently in this session come first.

Choosing an action runs it. Choosing a request file selects it in the sidebar. Choosing an environment makes it the active one for this session; the status bar shows it.

**Switch environment** (`Alt+E`) opens the same finder with environments only, the active one marked.

### Help

`?` (outside text inputs) or `F1` lists the keys for the focused context and the global ones, exactly as they are bound after your `keybindings.toml` (with vim keys on and the body focused, the modal editing keys too). `↑`/`↓` scroll; `Esc`, `?` or `F1` close it.

### Vim mode

`keymap = "vim"` in `tui.toml`, or `--vim`, adds vim keys on top of the defaults:

- In the sidebar and the response viewer: `j`/`k` move, `gg`/`G` jump to the top or bottom, `Ctrl+D`/`Ctrl+U` page, and `h`/`l` move to the pane on the left or right.
- The request body becomes modal. It starts in normal mode; `i`, `a` or `o` enter insert mode and `Esc` returns to normal mode; `v` starts a visual selection. The status bar shows `NORMAL`, `INSERT` or `VISUAL`.

This is not a full vim: there are no counts, registers, text objects, `:` commands or search in text areas.

Without vim mode the body is a plain text area: type, `Enter`, `Backspace`, `Delete`, arrows, `Home`/`End`. Until the request editor opens request files, the body is a scratch buffer that is not saved.

### `keybindings.toml`

Put your own bindings in `keybindings.toml` next to `tui.toml` (`~/.config/rustman/keybindings.toml` on Linux and macOS, `%APPDATA%\rustman\keybindings.toml` on Windows). Each table is a context; each entry maps keys to an action id, or to `"none"` to remove a binding:

```toml
[global]
"F5" = "request.send"     # F5 sends, in addition to Ctrl+Enter and Ctrl+R
"Ctrl+R" = "none"         # Ctrl+R no longer sends

[sidebar]
"Ctrl+J" = "list.down"
```

Your entries win over the vim preset, which wins over the defaults. Adding a key keeps the action's other keys; remove one with `"none"`. Help and the palette show your keys first.

Keys are written as modifiers and a key joined by `+`: `Ctrl`, `Alt` and `Shift` with a character, `Enter`, `Esc`, `Tab`, `Backspace`, `Delete`, `Insert`, `Home`, `End`, `PageUp`, `PageDown`, `Up`, `Down`, `Left`, `Right`, `Space` or `F1`–`F24` (`Ctrl+Enter`, `Alt+E`, `Shift+Tab`, `F5`). A shifted character is written as itself (`G`, `?`). Up to three keys in a row make a sequence: `"gg"`, or with spaces, `"Ctrl+K Ctrl+S"`. Many terminals can't tell `Ctrl+Enter` from `Enter`; that is why `Ctrl+R` also sends.

At start-up, every entry with an unknown action id, an unknown context or a key that can't be parsed is reported in a warning toast (and the notification log) and skipped; the other bindings work. A file that isn't valid TOML is ignored as a whole, with a warning.

Conflicts are reported the same way:

- Two spellings of the same key in one table (`"Ctrl+R"` and `"ctrl+r"`) are both skipped, since the file gives no order to pick one.
- A binding that is the start of a longer one (`"g"` when `gg` is bound) wins as soon as it is typed, so the longer one can't be typed there; the warning names both.
- A pane binding (`[editor]`, `[sidebar]`, `[response]`) wins over a `[global]` binding of the same keys while that pane is focused, so binding `"Ctrl+Q"` under `[editor]` to another action makes quitting impossible from the editor; the warning names both.
- If no key is left for `app.quit`, `Ctrl+Q` is bound to it again (replacing any other use of `Ctrl+Q`), so the TUI can always be closed.

### Key reference

The table below is generated from the action registry; a test fails if it drifts from the code.

`Esc` (`ui.escape`) closes overlays and dismisses toasts today. Once requests can be sent, it also cancels the request in flight in the current tab, like `Ctrl+C` (spec: Default keymap and Sending requests).

<!-- keymap:start: generated from the action registry, do not edit by hand -->

| Action | Id | Default keys | Vim preset adds |
|---|---|---|---|
| Send request | `request.send` | `Ctrl+Enter`, `Ctrl+R` |  |
| Cancel request | `request.cancel` | `Ctrl+C` |  |
| Save request | `request.save` | `Ctrl+S` |  |
| New tab | `tab.new` | `Ctrl+T` |  |
| Close tab | `tab.close` | `Ctrl+W` |  |
| Command palette | `palette.open` | `Ctrl+P` |  |
| Help | `help.open` | `?`, `F1` |  |
| Toggle sidebar | `sidebar.toggle` | `Ctrl+B` |  |
| Widen sidebar | `sidebar.grow` | `Alt+Right` |  |
| Narrow sidebar | `sidebar.shrink` | `Alt+Left` |  |
| Focus next | `focus.next` | `Tab` |  |
| Focus previous | `focus.prev` | `Shift+Tab` |  |
| Focus pane to the left | `pane.left` |  | `h` (response) |
| Focus pane to the right | `pane.right` |  | `l` (sidebar) |
| Close or dismiss | `ui.escape` | `Esc` |  |
| Switch environment | `env.switch` | `Alt+E` |  |
| Reveal secrets | `secrets.reveal` | `Alt+R` |  |
| Edit in external editor | `editor.external` | `Ctrl+E` (editor) |  |
| Open in viz companion | `viz.open` | `v` (response) |  |
| Show notification log | `notifications.show` |  |  |
| Move up | `list.up` | `Up` (sidebar, response) | `k` (sidebar, response) |
| Move down | `list.down` | `Down` (sidebar, response) | `j` (sidebar, response) |
| Go to top | `list.top` | `Home` (sidebar, response) | `gg` (sidebar, response) |
| Go to bottom | `list.bottom` | `End` (sidebar, response) | `G` (sidebar, response) |
| Page up | `list.page_up` | `PageUp` (sidebar, response) | `Ctrl+U` (sidebar, response) |
| Page down | `list.page_down` | `PageDown` (sidebar, response) | `Ctrl+D` (sidebar, response) |
| Quit | `app.quit` | `Ctrl+Q` |  |

In vim mode, text areas (the request body) are modal:

| Keys | What they do |
|---|---|
| `i` `a` `o` | insert before / after the cursor, on a new line below |
| `Esc` | back to normal mode |
| `h` `j` `k` `l` | left, down, up, right (also the arrow keys) |
| `w` `b` `e` | next word, previous word, end of word |
| `0` `$` | start, end of the line |
| `gg` `G` | first, last line |
| `Ctrl+D` `Ctrl+U` | down, up half a page |
| `x` | delete the character under the cursor |
| `dd` | cut the line |
| `yy` | copy the line |
| `p` | paste after the cursor |
| `u` | undo |
| `v` | visual mode: move to select |
| `y` `d` | in visual mode: copy, cut the selection |

<!-- keymap:end -->

## How it works

The UI follows an Elm-style loop (design D2 of the change): key presses, mouse events, resizes, clock ticks and finished background work all become actions on one channel; a pure `update` function changes the state and returns effects (load a workspace, create a workspace, and later send a request); an effect runner task executes those off the render loop and reports back with new actions; `view` draws the state. Nothing on the render loop waits for the disk or the network.

## Screenshots

To render one frame without a terminal, for example for the README:

```text
cargo run -p rustman-tui --example screenshot -- [--ansi] [--size WxH] [--theme NAME] [--ascii] [--no-color] [PATH]
```

It opens PATH as `rustman tui` would, runs the start-up work synchronously, and prints a 120x36 frame (or `--size`) as plain text, or with ANSI colours with `--ansi` (view it with `cat` or `less -R`). It ignores `tui.toml` and renders in 24-bit colour unless `--no-color` is given, so the output depends only on the flags and the workspace.
