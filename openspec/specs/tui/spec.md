# tui Specification

## Purpose
The interactive terminal front-end of Rustman: lets a human browse a workspace, compose and send requests, and inspect exchanges entirely from the keyboard, without ever blocking on the network.

## Requirements

### Requirement: Launching the TUI
Running `rustman` with no subcommand, or `rustman tui [PATH]`, SHALL open the TUI on the workspace found by searching PATH (default: current directory) and its ancestors for `rustman.toml`. If none is found, the TUI SHALL offer to create a workspace in PATH or open an empty scratch session. On exit the terminal SHALL be restored (cooked mode, main screen, cursor visible), including after a panic.

#### Scenario: Open workspace from a subdirectory
- **GIVEN** `/proj/rustman.toml` exists
- **WHEN** the user runs `rustman` from `/proj/api/users`
- **THEN** the TUI SHALL open the workspace rooted at `/proj`

#### Scenario: No workspace found
- **GIVEN** no `rustman.toml` exists in the current directory or any ancestor
- **WHEN** the user runs `rustman`
- **THEN** the TUI SHALL show a prompt with the choices "Create workspace here" and "Scratch session"

#### Scenario: Panic restores the terminal
- **GIVEN** the TUI is running
- **WHEN** an internal panic occurs
- **THEN** the terminal SHALL be restored to cooked mode on the main screen before the panic message is printed

### Requirement: Launching without a terminal
When stdin or stdout is not a terminal, `rustman` with no subcommand SHALL print the help and exit 0 instead of opening the TUI.

#### Scenario: No terminal
- **GIVEN** stdout is a pipe
- **WHEN** the user runs `rustman` with no subcommand
- **THEN** the help SHALL be printed and the exit status SHALL be 0

### Requirement: Three-region layout
At terminal widths of at least 120 columns the TUI SHALL show a collection tree sidebar (default 28 columns, resizable 20–50), a request editor and a response viewer side by side, plus a one-line status bar showing the workspace, active environment, TLS verification state and in-flight request count.

#### Scenario: Wide terminal
- **GIVEN** a terminal of 160x45
- **WHEN** the TUI opens
- **THEN** the sidebar, request editor and response viewer SHALL all be visible at once
- **AND** the status bar SHALL show the active environment name

### Requirement: Responsive collapse
Between 80 and 119 columns the sidebar SHALL become an overlay toggled with `Ctrl+B`, and the editor and response viewer SHALL stack vertically. Below 80 columns or 24 rows the TUI SHALL show one pane at a time with a pane switcher bar and a notice that the recommended minimum is 80x24. Resizing SHALL re-layout without losing state.

#### Scenario: 80x24 terminal
- **GIVEN** a terminal of 80x24
- **WHEN** the TUI opens
- **THEN** the editor and response viewer SHALL be stacked and the sidebar hidden
- **AND** `Ctrl+B` SHALL show the sidebar as an overlay

#### Scenario: Very small terminal
- **GIVEN** a terminal of 60x20
- **WHEN** the TUI opens
- **THEN** exactly one pane SHALL be visible with a pane switcher bar
- **AND** a notice SHALL state the recommended minimum of 80x24

#### Scenario: Resize keeps edits
- **GIVEN** an unsaved edit in the body editor
- **WHEN** the terminal is resized from 160x45 to 80x24
- **THEN** the unsaved edit SHALL still be present

### Requirement: Collection tree sidebar
The sidebar SHALL show the workspace's collections as a collapsible tree, with each request file labelled by its name and a coloured kind/method badge (e.g. `GET`, `POST`, `GQL`, `MCP`). `Enter` SHALL open the selected request in a tab. `/` SHALL fuzzy-filter the tree. `n`, `r` and `d` SHALL create, rename and delete (after confirmation) a request or folder.

#### Scenario: Open a request
- **GIVEN** the sidebar is focused on request `users/get-user`
- **WHEN** the user presses `Enter`
- **THEN** `get-user` SHALL open in a new tab, or the existing tab for it SHALL be focused

#### Scenario: Filter the tree
- **WHEN** the user presses `/` and types `gusr`
- **THEN** only requests whose path fuzzy-matches `gusr` SHALL remain visible, with matched characters highlighted

#### Scenario: Delete requires confirmation
- **WHEN** the user presses `d` on a request
- **THEN** a confirmation dialog SHALL appear and the file SHALL be deleted only after the user confirms

### Requirement: HTTP request editor tabs
For request files of kind `http` the editor SHALL offer the tabs Params, Headers, Body, Auth and Assertions, with the method selector and URL input above them. Params and Headers SHALL be editable key/value tables with per-row enable toggles. Body SHALL support the modes none, raw (JSON, XML, text), form-urlencoded and multipart. Edits to the URL query and the Params table SHALL stay in sync.

#### Scenario: Query string and Params stay in sync
- **GIVEN** the URL `https://api.test/users`
- **WHEN** the user adds the Params row `page` = `2`
- **THEN** the URL SHALL read `https://api.test/users?page=2`

#### Scenario: Disabled header not sent
- **GIVEN** the header row `X-Debug: 1` is toggled off
- **WHEN** the request is sent
- **THEN** `X-Debug` SHALL NOT be included in the request

### Requirement: Variable highlighting in editors
Every `{{variable}}` in the URL, headers, params and body SHALL be highlighted: resolved variables in the accent colour, unresolved ones in the error colour. Hovering or placing the cursor on a variable SHALL show its resolved value and its source layer. Secret values SHALL be shown masked.

#### Scenario: Unresolved variable
- **GIVEN** the URL `{{base_url}}/users` and no `base_url` in any layer
- **WHEN** the editor renders
- **THEN** `{{base_url}}` SHALL be highlighted in the error colour

#### Scenario: Inspect a secret
- **GIVEN** the header `Authorization: Bearer {{token}}`, where `token` is a secret
- **WHEN** the cursor is on `{{token}}`
- **THEN** the tooltip SHALL show the source as secret and the value as `••••••••`

### Requirement: Multiple request tabs
The TUI SHALL allow several requests open as tabs. `Ctrl+T` SHALL open a new scratch request, `Ctrl+W` SHALL close the current tab, and `Alt+1`…`Alt+9` and `Ctrl+PageUp`/`Ctrl+PageDown` SHALL switch tabs. Each tab SHALL keep its own editor state, last exchange and in-flight status.

#### Scenario: Independent tabs
- **GIVEN** two open tabs, A with a request in flight and B idle
- **WHEN** the user switches to B and edits its URL
- **THEN** A's request SHALL continue and its response SHALL appear in A when it completes

### Requirement: Unsaved change tracking
A tab whose request differs from its file on disk SHALL show a `●` marker in its title and in the sidebar. `Ctrl+S` SHALL save the request file. Closing a dirty tab or quitting with dirty tabs SHALL prompt Save / Discard / Cancel. If the file changes on disk while the tab is clean, the tab SHALL reload it; if the tab is dirty, the TUI SHALL warn of the conflict.

#### Scenario: Dirty marker
- **GIVEN** a saved request open in a tab
- **WHEN** the user changes its URL
- **THEN** the tab title SHALL show `●`
- **AND** after `Ctrl+S` the marker SHALL disappear

#### Scenario: Quit with unsaved changes
- **GIVEN** a dirty tab
- **WHEN** the user presses `Ctrl+Q`
- **THEN** a Save / Discard / Cancel prompt SHALL appear before exiting

#### Scenario: External edit conflict
- **GIVEN** a dirty tab for `get-user`
- **WHEN** `get-user.rman.yaml` is modified by another program
- **THEN** the TUI SHALL show a conflict warning offering "Keep mine" or "Load from disk"

### Requirement: Non-blocking send
Sending (`Ctrl+Enter`, fallback `Ctrl+R`) SHALL start the request in the background and return control immediately. While in flight, the tab SHALL show a spinner and a live elapsed-time counter. Input, navigation, rendering and other tabs SHALL remain responsive throughout; no network or disk I/O SHALL run on the render loop.

#### Scenario: UI stays responsive during a slow request
- **GIVEN** a server that responds after 10 seconds
- **WHEN** the user sends a request and then presses `Ctrl+B` and moves through the sidebar
- **THEN** each keypress SHALL be reflected on screen within 50 ms
- **AND** the spinner and elapsed counter SHALL keep updating

### Requirement: Cancel in-flight request
`Esc` or `Ctrl+C` in a tab with a request in flight SHALL cancel it, close the connection and record the exchange as `cancelled` with the elapsed time. `Ctrl+C` with nothing in flight SHALL NOT quit the application; quitting SHALL be `Ctrl+Q`.

#### Scenario: Cancel a hanging request
- **GIVEN** a request in flight for 3 seconds
- **WHEN** the user presses `Esc`
- **THEN** the spinner SHALL stop, the response viewer SHALL show `Cancelled after 3.0 s`, and the connection SHALL be closed

#### Scenario: Ctrl+C when idle
- **GIVEN** no request in flight
- **WHEN** the user presses `Ctrl+C`
- **THEN** the TUI SHALL keep running and show the toast `Press Ctrl+Q to quit`

### Requirement: Response summary line
After an exchange completes, the response viewer SHALL show a summary line with the status code and reason coloured by class (2xx ok, 3xx accent, 4xx warn, 5xx error), the total time, the body size, and the assertion tally (e.g. `4/5 passed`). Transport failures SHALL show the error category and message instead of a status.

#### Scenario: Successful exchange
- **GIVEN** a response `201 Created` of 1.2 KiB in 84 ms, with 3 of 3 assertions passing
- **THEN** the summary SHALL read `201 Created · 84 ms · 1.2 KiB · 3/3 passed`, with the status in the ok colour

#### Scenario: DNS failure
- **GIVEN** the host does not resolve
- **THEN** the summary SHALL show `DNS error` and the resolver message, with no status code

### Requirement: Response viewer tabs
The response viewer SHALL offer the tabs Body, Headers, Timing and Assertions. Headers SHALL list all response headers in received order, including duplicates. Assertions SHALL list each assertion with pass/fail, expected and actual values. Captured variables SHALL be listed with their new values, masked when secret.

#### Scenario: Duplicate response headers
- **GIVEN** a response with two `Set-Cookie` headers
- **WHEN** the user opens the Headers tab
- **THEN** both `Set-Cookie` entries SHALL be listed

#### Scenario: Failed assertion details
- **GIVEN** the assertion `status == 200` and an actual status of 404
- **WHEN** the user opens the Assertions tab
- **THEN** the row SHALL show failed, expected `200` and actual `404`

### Requirement: JSON tree viewer
JSON bodies SHALL render as a collapsible tree with syntax colours. `Enter`/`Space` SHALL toggle a node, `zc`/`zo` (or `←`/`→`) SHALL collapse/expand, `zM`/`zR` SHALL collapse/expand all, and `y` SHALL copy the selected node's value. Large arrays SHALL show their element count when collapsed. `r` SHALL toggle between the tree and raw pretty-printed text.

#### Scenario: Collapse a large array
- **GIVEN** a body `{"items": [ ...1000 objects... ]}`
- **WHEN** the user collapses `items`
- **THEN** the row SHALL read `items: [1000 items]`

#### Scenario: Copy a value
- **GIVEN** the node `$.user.id` with value `42` is selected
- **WHEN** the user presses `y`
- **THEN** `42` SHALL be placed on the system clipboard (or via OSC 52 when no clipboard is available) and a toast SHALL confirm the copy

### Requirement: JSON filter and search
In the Body tab, `f` SHALL open a filter box accepting a JSONPath expression (RFC 9535). The tree SHALL show only the matching nodes and the match count. An invalid expression SHALL show its parse error inline without clearing the view. `/` SHALL search keys and values incrementally, with `n`/`N` moving between matches.

#### Scenario: Filter with JSONPath
- **GIVEN** a body with `items` holding 3 objects with `id` 1, 2, 3
- **WHEN** the user enters the filter `$.items[*].id`
- **THEN** the viewer SHALL show `1`, `2`, `3` and `3 matches`

#### Scenario: Invalid filter
- **WHEN** the user enters `$.items[`
- **THEN** an inline parse error SHALL be shown and the previous view SHALL remain

### Requirement: Highlighting for non-JSON bodies
Non-JSON bodies SHALL be highlighted according to the response `Content-Type`: HTML, XML, YAML, JavaScript and CSS. Others SHALL show as plain text. Binary bodies (non-text content types, or invalid UTF-8) SHALL show a hex dump of the first 4 KiB plus the total size, and an action to save the body to a file.

#### Scenario: XML response
- **GIVEN** a response with `Content-Type: application/xml`
- **THEN** the body SHALL be shown with XML syntax highlighting

#### Scenario: Image response
- **GIVEN** a response with `Content-Type: image/png` of 300 KiB
- **THEN** the Body tab SHALL show a hex dump, `300 KiB`, and a "Save body…" action

### Requirement: Large response handling
Bodies larger than 10 MiB (configurable as `max_inline_body`) SHALL be spooled to a temp file and viewed lazily. Above 50 MiB the tree view SHALL be disabled in favour of a paged raw view, and the TUI SHALL say why. Scrolling SHALL stay responsive regardless of body size.

#### Scenario: 80 MiB JSON response
- **GIVEN** an 80 MiB JSON response
- **WHEN** the Body tab opens
- **THEN** a paged raw view SHALL be shown with a notice that the tree view is disabled above 50 MiB

### Requirement: Timing waterfall
The Timing tab SHALL show a horizontal waterfall bar of the exchange phases reported by `http-requests` (DNS, connect, TLS, request sent, waiting/TTFB, content download), proportional to duration, with each phase's milliseconds labelled. Phases that did not occur (e.g. reused connection) SHALL be shown as `—`.

#### Scenario: Reused connection
- **GIVEN** an exchange over a reused keep-alive connection
- **WHEN** the user opens the Timing tab
- **THEN** DNS, connect and TLS SHALL show `—`, and waiting and download SHALL show their durations

### Requirement: Latency sparkline from history
The Timing tab SHALL show a sparkline of the total time of the last 30 exchanges for the same request file, taken from `request-history`, with min, median, p95 and max labelled. The current exchange SHALL be highlighted. Fewer than 2 data points SHALL show `not enough history`.

#### Scenario: Sparkline with history
- **GIVEN** 30 prior exchanges of `get-user`
- **WHEN** the user opens the Timing tab after a new send
- **THEN** a sparkline of 30 points SHALL be shown with min, median, p95 and max, and the newest point highlighted

### Requirement: Diff two exchanges
`D` in the response viewer SHALL open a picker of earlier exchanges (same request first, then any) and show a diff against the current one. Status, headers and body SHALL be compared; JSON bodies SHALL be diffed structurally by path with key order ignored, and other bodies line by line.

#### Scenario: Structural JSON diff
- **GIVEN** exchange A with body `{"a":1,"b":2}` and exchange B with body `{"b":3,"a":1}`
- **WHEN** the user diffs A against B
- **THEN** the diff SHALL report only `$.b: 2 → 3`

### Requirement: Environment switcher
The environment switcher SHALL open with `Alt+E` and list all environments, with the active one marked. Selecting one SHALL make it active for subsequent sends and update the status bar. The switcher SHALL show each variable's name, layer and value.

#### Scenario: Switch environment
- **GIVEN** environments `dev` (active) and `staging`
- **WHEN** the user opens the switcher with `Alt+E` and selects `staging`
- **THEN** the status bar SHALL show `staging` and the next send SHALL resolve variables from `staging`

### Requirement: Secret masking and reveal
Secret values SHALL be rendered as `••••••••` in editors, tooltips, the environment switcher and captured values; response data echoing a secret SHALL show the engine's marker `«redacted:<name>»`, as history does, even while revealed. A reveal toggle (`Alt+R`) SHALL unmask secrets for the current session only, show a persistent `SECRETS VISIBLE` badge in the status bar while active, and reset on restart.

#### Scenario: Echoed secret in a response
- **GIVEN** the secret `token` is sent and the server echoes it in the response
- **WHEN** the response viewer shows the echoed value
- **THEN** it SHALL show `«redacted:token»` and never the secret value

#### Scenario: Reveal is session-only
- **GIVEN** the user revealed secrets with `Alt+R`
- **WHEN** the TUI is restarted
- **THEN** secrets SHALL be masked again

#### Scenario: Badge while revealed
- **WHEN** secrets are revealed
- **THEN** the status bar SHALL show `SECRETS VISIBLE` in the warn colour until they are masked again

### Requirement: Insecure TLS indicator
When TLS certificate verification is disabled for the active request or for the workspace every environment sends with, the status bar SHALL show a persistent `TLS: INSECURE` badge in the error colour, and each exchange sent that way SHALL be marked insecure in its summary line.

#### Scenario: Verification disabled
- **GIVEN** the workspace disables TLS verification (`verify_tls = false` in `rustman.toml`)
- **THEN** the status bar SHALL show `TLS: INSECURE` for as long as it stays disabled

#### Scenario: Verification disabled for one request
- **GIVEN** the request in the active tab sets `verify_tls: false`
- **WHEN** that tab is active
- **THEN** the status bar SHALL show `TLS: INSECURE`, and SHALL stop showing it when a tab without that setting is active

### Requirement: Command palette
`Ctrl+P` SHALL open a fuzzy command palette listing every action (with its current key binding), every request file and every environment. Typing SHALL rank entries by fuzzy match, `Enter` SHALL run the selected entry, and `Esc` SHALL close the palette. Recently used entries SHALL rank first when the query is empty.

#### Scenario: Run an action from the palette
- **WHEN** the user presses `Ctrl+P`, types `swenv` and presses `Enter` on "Switch environment"
- **THEN** the environment switcher SHALL open

#### Scenario: Palette shows bindings
- **WHEN** the palette lists "Send request"
- **THEN** the entry SHALL display its current binding, e.g. `Ctrl+Enter`

### Requirement: Default keymap
The TUI SHALL ship a default keymap that includes at least: `Ctrl+Enter`/`Ctrl+R` send, `Esc` cancel, `Ctrl+S` save, `Ctrl+T` new tab, `Ctrl+W` close tab, `Ctrl+P` palette, `Ctrl+B` sidebar, `Alt+E` environments, `Alt+R` reveal secrets, `Ctrl+E` external editor, `Tab`/`Shift+Tab` focus cycling, `?` help, `v` viz, `Ctrl+Q` quit.

#### Scenario: Focus cycling
- **GIVEN** focus is on the sidebar
- **WHEN** the user presses `Tab` repeatedly
- **THEN** focus SHALL move through sidebar, method, URL, editor tabs, editor content and response viewer, then back to the sidebar

### Requirement: Vim mode
Setting `keymap = "vim"` in `tui.toml` (or `--vim`) SHALL enable vim-style navigation: `h j k l` movement, `gg`/`G`, `Ctrl+D`/`Ctrl+U` paging, and modal editing in text areas (normal, insert and visual modes, with `i a o x dd yy p u` and `w b e` motions). The current mode SHALL be shown in the status bar.

#### Scenario: Modal editing in the body
- **GIVEN** vim mode is enabled and the body editor is focused in normal mode
- **WHEN** the user types `i`, then `abc`, then `Esc`
- **THEN** `abc` SHALL be inserted and the status bar SHALL show `NORMAL`

### Requirement: Remappable keybindings
The TUI SHALL load user overrides from `keybindings.toml` in the platform config directory (e.g. `~/.config/rustman/`). Each entry SHALL map a context and key chord to an action id. User entries SHALL override the vim preset, which overrides the defaults. Unknown action ids or unparsable chords SHALL be reported in a toast at startup and skipped.

#### Scenario: Rebind send
- **GIVEN** `keybindings.toml` contains `[global] "F5" = "request.send"`
- **WHEN** the user presses `F5` in the editor
- **THEN** the current request SHALL be sent

#### Scenario: Invalid entry
- **GIVEN** `keybindings.toml` maps `F6` to `request.sned`
- **WHEN** the TUI starts
- **THEN** a toast SHALL report the unknown action `request.sned` and all other bindings SHALL work

### Requirement: Help overlay
`?` (outside text inputs) or `F1` SHALL show a help overlay listing the bindings for the focused context and the global ones, as currently effective after user overrides. `Esc` or `?` SHALL close it.

#### Scenario: Help reflects overrides
- **GIVEN** the user rebound send to `F5`
- **WHEN** the user opens the help overlay
- **THEN** "Send request" SHALL be listed with `F5`

### Requirement: Toasts
Transient notifications SHALL appear as toasts in the top-right corner, stacked at most 3 deep, with severity styling (info, success, warn, error). Info and success toasts SHALL auto-dismiss after 3 seconds; warn and error toasts SHALL stay until dismissed or 10 seconds pass. A notification log SHALL be viewable from the palette.

#### Scenario: Error toast persists
- **WHEN** saving a request file fails with a permission error
- **THEN** an error toast SHALL appear with the OS error and stay for up to 10 seconds
- **AND** the message SHALL be available in the notification log

### Requirement: Themes and colour capability
The TUI SHALL ship the themes `dark` (default), `light` and `high-contrast`, selectable with `--theme` or in `tui.toml`. It SHALL use 24-bit colour when `COLORTERM` is `truecolor` or `24bit`, and the nearest xterm-256 colours otherwise. When `NO_COLOR` is set to a non-empty value, it SHALL render without colour and convey status with symbols and text attributes.

#### Scenario: 256-colour terminal
- **GIVEN** `COLORTERM` is unset and `TERM=xterm-256color`
- **WHEN** the TUI renders
- **THEN** every theme colour SHALL be mapped to its nearest xterm-256 index

#### Scenario: NO_COLOR
- **GIVEN** `NO_COLOR=1`
- **WHEN** a `500` response is shown
- **THEN** the summary SHALL render without colour and include the `✖` symbol before the status

### Requirement: ASCII fallback
Setting `borders = "ascii"` in `tui.toml` (or `--ascii`) SHALL replace box-drawing and other non-ASCII glyphs (borders, spinner, sparkline blocks, tree arrows, badges) with ASCII equivalents.

#### Scenario: ASCII borders
- **GIVEN** `--ascii`
- **WHEN** the TUI renders
- **THEN** pane borders SHALL use only `+`, `-` and `|`

### Requirement: Mouse support
When the terminal reports mouse events, the TUI SHALL support clicking to focus panes, select tree items and tabs, and press buttons; the wheel to scroll; and dragging the sidebar border to resize. Mouse support SHALL be disableable with `mouse = false` so the terminal's native text selection works.

#### Scenario: Click a tab
- **WHEN** the user clicks the Headers tab label
- **THEN** the Headers tab SHALL become active

#### Scenario: Mouse disabled
- **GIVEN** `mouse = false` in `tui.toml`
- **THEN** the TUI SHALL NOT capture mouse events

### Requirement: External editor for bodies
`Ctrl+E` in the Body tab SHALL write the body to a temp file with an extension matching its content type, suspend the TUI, and open `$VISUAL`, else `$EDITOR`, else `vi` (`notepad` on Windows). When the editor exits, the TUI SHALL resume and load the file's contents into the body, marking the tab dirty if they changed. The temp file SHALL then be deleted.

#### Scenario: Edit JSON body externally
- **GIVEN** `EDITOR=nvim` and a JSON body
- **WHEN** the user presses `Ctrl+E`, edits and saves in nvim, and quits
- **THEN** the TUI SHALL resume with the edited body and a dirty marker on the tab

### Requirement: Protocol pane extension point
The request editor and response viewer SHALL choose their tabs from a pane registered for the request file's `kind`. This change SHALL register the `http` pane. A request file whose `kind` has no registered pane SHALL open as a read-only raw view with a notice naming the missing kind.

#### Scenario: Unknown kind
- **GIVEN** a request file with `kind: grpc` and no pane registered for it
- **WHEN** the user opens it
- **THEN** the file SHALL be shown read-only with the notice `No editor for kind "grpc"`

### Requirement: Open in viz companion
`v` in the response viewer SHALL open the current exchange in the viz companion, and in a run view SHALL open the run dashboard, using the discovery defined by `visual-dashboards`. If no companion is available, the TUI SHALL show a toast explaining why and offering to export an HTML report instead.

#### Scenario: Companion not installed
- **GIVEN** no `rustman-viz` binary can be discovered
- **WHEN** the user presses `v`
- **THEN** a toast SHALL say `Viz companion not found` and offer "Export HTML report"

### Requirement: Copy as picker
`Alt+C` and the palette action "Copy as…" SHALL open a picker listing the snippet languages from `http-requests`, with the most recently used one first. `Enter` SHALL generate the snippet for the focused request and copy it. Each language SHALL also appear in the palette as "Copy as <language>".

#### Scenario: Copy as Python
- **GIVEN** a focused HTTP request
- **WHEN** the user presses `Alt+C`, types `py req` and presses `Enter`
- **THEN** the python-requests snippet SHALL be copied and the toast `Copied python-requests snippet` SHALL appear

#### Scenario: Unsupported kind
- **GIVEN** a focused `kafka` request
- **WHEN** the user presses `Alt+C`
- **THEN** a toast SHALL read `No snippet export for kind 'kafka'` and no picker SHALL open

### Requirement: Clipboard via OSC 52
Copying SHALL use the OSC 52 escape sequence, which works over SSH and inside tmux. If the terminal doesn't advertise support, or the payload is over 100 KB, the snippet SHALL open in a read-only pane with the hint `Select to copy`.

#### Scenario: Over SSH
- **GIVEN** Rustman runs over SSH in a terminal supporting OSC 52
- **WHEN** a snippet is copied
- **THEN** the local machine's clipboard SHALL contain the snippet

#### Scenario: Unsupported terminal
- **GIVEN** a terminal without OSC 52 support
- **WHEN** a snippet is copied
- **THEN** the read-only snippet pane SHALL open instead

### Requirement: Secrets in copied snippets
Copied snippets SHALL be redacted by default. While secrets are revealed with `Alt+R`, the picker SHALL show a warning row `Secrets revealed: snippet will contain secret values`, and SHALL require pressing `Enter` twice to copy.

#### Scenario: Copy while revealed
- **GIVEN** secrets are revealed
- **WHEN** the user selects a language and presses `Enter` once
- **THEN** nothing SHALL be copied until a second `Enter` confirms
