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

The sidebar shows the workspace as a collection tree, the request editor edits the open request (see [Requests](#requests)) and the response viewer shows its last exchange (see [Response viewer](#response-viewer)).

### Focus

`Tab` and `Shift+Tab` move the focus around a ring: sidebar, method, URL, editor tabs, editor content, response viewer, and back to the sidebar. A hidden sidebar is skipped. The focused pane has a highlighted border and a `›` before its title.

### Status bar

The bottom line shows, from the left: the workspace name (or `Scratch session`), the active environment (`env: NAME` or `no environment`), `TLS verified` or a red `TLS: INSECURE` badge while certificate verification is off (`verify_tls = false` in `rustman.toml`, or `settings: verify_tls: false` in the request open in the current tab), and how many requests are in flight. On the right: the vim mode (`NORMAL`, `INSERT`, `VISUAL`) when vim keys are on, a `SECRETS VISIBLE` badge in the warning colour while secrets are revealed, the webhook listener's address while it runs (`webhook 127.0.0.1:9090`, or a red `WEBHOOK ON NETWORK 0.0.0.0:9090` badge when it is reachable from the network; see [Webhook pane](#webhook-pane)), the recording proxy's address while it runs (`record 127.0.0.1:8888`, or a red `RECORDER ON NETWORK` badge) and a `CA TRUSTED` badge while the workspace's recorder CA is installed in a trust store (see [Recorder pane](#recorder-pane)), and the quit hint.

### Toasts and the notification log

Messages appear as toasts in the top-right corner, newest on top, at most three at a time. Info and success toasts disappear after 3 seconds; warnings and errors stay until dismissed or for 10 seconds. `Esc` dismisses the newest toast, and clicking a toast dismisses it. Every toast is also kept in the notification log (the last 500), which **Show notification log** in the command palette opens; `Esc` closes it.

Warnings found while loading the workspace, a `tui.toml` that can't be read, and bad entries in `keybindings.toml` are shown as warning toasts.

## Requests

### Collection tree

The sidebar shows the workspace's folders and request files in run order (the order `rustman run` uses). Each request has a badge: its method (`GET` in the ok colour, `POST` accent, `PUT`/`PATCH` warn, `DELETE` error) or its kind for other request kinds (`GQL`, `MCP`, `GRPC`, ...). A `●` (`*` with `--ascii`) marks a request open in a tab with unsaved changes.

| Key (sidebar) | What it does |
|---|---|
| `↑`/`↓`, `Home`/`End`, `PageUp`/`PageDown` | move the selection |
| `Enter` | open the request in a tab (or focus its tab); on a folder, fold or unfold it |
| `←` / `→` | fold / unfold the selected folder |
| `/` | filter: type to keep the requests whose path fuzzy-matches (matched characters are highlighted), `Enter` opens the selection, `Esc` clears the filter |
| `n` | new request in the selected folder; a name such as `admin/list` also creates the folder |
| `r` | rename the selected request or folder (open tabs follow) |
| `d` | delete the selected request or folder, after a confirmation (Cancel is preselected) |

Request kinds this build does not know (for example `kind: soap`) still load: they open read-only, showing the file as it is, with the notice `No editor for kind "soap"`.

### Tabs

Several requests can be open at once; their tabs are listed in the request editor's top border, the current one highlighted. `Ctrl+T` opens a new request, `Ctrl+W` closes the current tab, `Alt+1` … `Alt+9` go to a tab, and `Ctrl+PageUp` / `Ctrl+PageDown` go to the previous / next one (clicking a tab works too). Each tab keeps its own editor state, its last exchange and its in-flight request, so a request can keep running in one tab while you edit another.

### Saving and unsaved changes

A tab whose request differs from its file shows `●` after its title, and the request shows `●` in the sidebar. `Ctrl+S` saves it (`Ctrl+S` is safe to use: raw mode turns off XON/XOFF flow control); a new tab asks for a file name first, relative to the workspace root (`users/create`, `.rman.yaml` is added). Files are written in Rustman's canonical layout: a query string typed in the URL is saved as `query:` rows, and settings, variables, captures and descriptions are kept as they were. In a scratch session nothing is saved.

Closing a tab with unsaved changes, or quitting with any, asks **Save / Discard / Cancel** (`s`, `d`, `c`, or the arrows and `Enter`; `Esc` cancels).

The TUI watches the workspace for changes made by other programs (an editor, `git checkout`). The sidebar follows them. A tab without unsaved changes reloads its file; a tab with unsaved changes shows a conflict prompt: **Keep mine** (keep editing; saving overwrites the file) or **Load from disk**.

### The HTTP editor

The first row holds the method and the URL. With the method focused, `←`/`→`, `Space` or `Enter` cycle through `GET POST PUT PATCH DELETE HEAD OPTIONS`, and a letter jumps to the next method starting with it. The URL is a one-line text field.

Below them, the editor tabs: `Params`, `Headers`, `Body`, `Auth` and `Assertions` (with the tab bar focused, `←`/`→` or `1`–`5` switch; clicking a label does too).

- **Params** and **Headers** are tables with an enable toggle per row. Keys in a table: `Enter` edits the name, then `Tab` moves to the value and `Enter` commits (`Esc` cancels the edit); `Space` turns the row on or off; `a` (or `Insert`) adds a row; `x` (or `Delete`) removes it; `↑`/`↓` move. A disabled row stays in the file (`enabled: false`) but is not sent. Params and the URL's query string stay in sync both ways: adding the row `page` = `2` to `https://api.test/users` makes the URL `https://api.test/users?page=2`, and typing `?a=1` in the URL adds the row; disabled rows are left out of the URL.
- **Body**: `Alt+M` cycles the mode: `none`, `raw (JSON)`, `raw (XML)` (adds `Content-Type: application/xml` when no content type is set), `raw (text)`, `form-urlencoded` and `multipart`. Raw bodies are text areas (modal with vim keys); form and multipart bodies are tables, where a multipart value `@path` sends a workspace file. A `type: file` body is shown, not edited.
- **Auth**: `Alt+M` cycles `inherit` (no `auth:`, the nearest collection's applies), `none`, `basic`, `bearer` and `api_key`; their fields form a table. OAuth2 is shown and kept as is; edit it in the file.
- **Assertions**: the request's `assertions:` as YAML, the same syntax as in the file (see [Assertions](assertions.md)). A list that does not parse cannot be saved: the error names the problem.

### The GraphQL editor

A `kind: graphql` request (see [GraphQL](graphql.md)) opens in its own editor. The first row shows how the operation travels (`POST`, `GET`, `WS` or `SSE`) and the endpoint, a one-line text field. The editor tabs are `Query`, `Variables` and `Schema` (`1`–`3` with the tab bar focused).

- **Query** is the document, with syntax colours. Its first line says where the schema comes from (`Schema: schema_file schema.graphql · 42 types`, `Schema: cache, fetched …`), or `No schema: syntax only` when there is none. The document is checked as you type, without sending: every problem is underlined where it starts (in the error colour, the warning colour for deprecated fields) and the last line shows the one at the cursor, e.g. `✖ 2:8: Cannot query field "emial" on type "User".`, or `✔ Valid`. A document from `document_file` is shown read-only; edit the file.
- `Ctrl+Space` opens the completion popup at the cursor: fields, arguments, enum values, variables, directives and fragments, each with its signature. `↑`/`↓` choose, `Enter` or `Tab` inserts, typing narrows the list and `Esc` closes it.
- **Variables** is the `variables:` map as a JSON object; `{{variables}}` in it are resolved on send. JSON that does not parse is reported on the last line, and the request cannot be saved or sent until it does.
- **Schema** browses the schema: the types with their kind, then (`Enter`) a type's fields with their arguments and types, input fields, enum values, interfaces and possible types; deprecated members are struck through with their reason. `Enter` on a field opens its type; `Backspace` goes back.

The schema is the request's `schema_file`, the `schema_file` of its endpoint in `rustman.toml`, or the cached introspection result, read when the tab opens. Opening or sending never introspects: `Ctrl+G` (**Refresh GraphQL schema**) runs the introspection query against the endpoint, in the active environment, and replaces the cache. Edits are saved to the request file with `Ctrl+S`; fields the editor does not show (headers, auth, transport, assertions) are kept.

Sending a document with errors asks first: **Send anyway** sends it without the check, **Cancel** (the default) does not send. A query or mutation shows its result in the response viewer's `Data`, `Errors` and `Extensions` tabs. Each of these tabs starts with a summary line: the operation, the result (`ok`, `partial`, `errors`), the subscription protocol (`graphql-transport-ws`, `legacy subscriptions-transport-ws` or `sse`) and the APQ outcome, when there are any (`subscription OnMessage · ok · legacy subscriptions-transport-ws`). `Alt+S` (**Start GraphQL subscription**, or `Ctrl+Enter`) starts a `transport: ws` or `sse` subscription; it runs until the server completes it or you stop it, with no `max_events` or `duration` limit. While it runs, the response viewer counts the events and lists the newest (`⠋ Subscribed: 3 events · 2.4 s`). `Alt+X` (**Stop GraphQL subscription**) stops it and keeps what arrived: the `Events` tab lists every event with its time since the start and why it ended (`Ended: stopped, 3 events`). `Esc` cancels it instead, like any request.

### The JSON-RPC editor

A `kind: jsonrpc` request (see [JSON-RPC](jsonrpc.md#in-the-tui)) opens in its own editor. The first row shows `RPC` (or `SUB` for a subscription) and the endpoint. The editor tabs are `Call`, `Batch`, `Headers`, `Auth` and `Assertions` (`1`–`5` with the tab bar focused).

- **Call** starts with where method completions come from (`Catalog: 42 methods`, from `[jsonrpc.catalogs]` or the `rustman rpc discover` cache; the endpoint is resolved in the active environment, without reading secrets), then the one-line **Method** field and the **Params** as JSON (an array or an object). Typing in the method field opens a popup of the catalog methods starting with what is typed, each with its param names (`eth_getBalance(address, block?)`, `?` marking optional ones); `Ctrl+Space` opens it too. `↑`/`↓` choose, `Enter` or `Tab` takes one, `Esc` closes it. `Enter` or `↓` moves from the method to the params, `↑` on the first params row moves back. Params that do not parse are reported on the last line. A batch file shows its calls in the **Batch** tab instead.
- **Batch**, **Headers**, **Auth** and **Assertions** are those parts of the file as YAML, the same syntax as in the file. YAML that does not parse, or a file that no longer loads (for example a method left empty), cannot be saved or sent: the error says why.

An edited request is saved with its members in schema order; an unedited one keeps its file text. `Ctrl+R` sends it; the response viewer shows `Result` (each call's result exactly as received, hex included), `Error` (code, message and `data`, and `server rejected the batch` when one error answered a whole batch) and `Events`, then the usual `Body`, `Headers`, `Timing` and `Assertions`. A subscription runs until you stop it: the viewer counts the events and lists the newest live (`⠋ Subscribed: 3 events · 2.4 s  (Esc unsubscribes)`). `Esc` sends the `unsubscribe` call and stops it, keeping what arrived (`Ended: stopped, 3 events`); a second `Esc` cancels outright. A method guard that refuses the method stops the send with its message.

### The gRPC editor

A `kind: grpc` request (see [gRPC](grpc.md#in-the-tui)) opens in its own editor. The first row shows `GRPC` and the target. The editor tabs are `Method`, `Message`, `Metadata` and `Assertions` (`1`–`4` with the tab bar focused).

- **Method** starts with where the schema comes from: the request's `.proto` files or descriptor set, else the [descriptor cache](grpc.md#descriptor-cache), else the server by reflection (whose answer is then cached). It loads in the background when the tab opens, with the request resolved in the active environment, so errors are redacted. `Ctrl+G` asks the server again. When the server cannot be reached and the cache has the target, the cached schema stands in and the line says so: `Schema (may be stale, Ctrl+G refreshes): 5 methods from the descriptor cache, fetched …`. Below the picked method and its call type, the browser lists every method of every service with its call type; `↑`/`↓` select and `Enter` picks one.
- **Message** is the request message as JSON, in the proto3 JSON mapping, with its request type on the first line. Picking a method puts its template in the tab (every field at its default, nested messages to depth 3, repeated fields as one-element lists, enums as their first value) unless you typed something there; a client-streaming or bidi method takes a list (`messages`), sent in order. JSON that does not parse is reported on the last line.
- **Metadata** and **Assertions** are those parts of the file as YAML.

`Ctrl+R` sends the request. While a call streams, the response pane counts the messages and lists the newest live, each with its direction (`>` sent, `<` received), time since the call started, size and JSON (`⠋ 5 messages · 2.4 s  (Ctrl+R sends, Alt+X half-closes)`). Interactive streams have no `stream` bounds: they run until the server ends them, the deadline expires, or you stop them. During a bidi call, `Ctrl+R` sends the Message tab (each entry of a list) on the open stream instead of starting a new call; a message the schema rejects is not sent and the toast says why. `Alt+X` half-closes the stream: the server may still answer, and the call ends when it does. `Esc` cancels the call outright.

The response viewer shows `Messages` (each received message), `Status` (the status code, name and message, the decoded `google.rpc.Status` details such as `BadRequest` field violations, an unknown detail as its type URL and base64, then the response headers and trailers), `Timeline` and `Assertions`. With the response focused, `d` cycles the timeline between all, sent and received messages, and `f` filters it by text as you type (`Enter` keeps the filter, `Esc` clears it); the filter line shows how many entries match (`Direction: received (d) · Filter: more (f) · 1 of 5`). Every value is redacted like history: a secret sent in metadata and echoed by the server shows `«redacted:token»`.

### The WebSocket and SSE editor

A `kind: ws` or `kind: sse` request (see [WebSocket and SSE](realtime.md#in-the-tui)) opens in the realtime editor. The first row shows `WS` or `SSE` and the URL. The editor tabs are `Timeline`, `Compose`, `Headers`, `Steps` (WebSocket) or `Body` (SSE), and `Assertions` (`1`–`5` with the tab bar focused).

- **Timeline** shows the session: `Connected · 12 of 80 entries (3 dropped)`, the filters, one row per entry (time since the session started, `sent`, `recv` or `sys`, the type, the size and the start of the payload) and, below, the payload of the selected entry, pretty-printed when it is JSON. With the content focused, `d` cycles the direction filter (all, sent, received, system), `t` the type filter (each type seen so far), `/` starts typing a text filter (`Enter` ends it), `c` clears the filters, `↑`/`↓` (or `k`/`j`) select an entry, `g` goes to the first and `G` (or `End`) follows the newest again. Only the rows that fit are drawn, and the pane keeps at most `limits.max_timeline` entries (default 10000), so a long session never slows down input.
- **Compose** holds the frame to send: text, JSON (checked before it is sent) or binary as base64; `Alt+M` changes the type. It is sent as typed, without interpolation.
- **Headers**, **Steps** / **Body** and **Assertions** are those parts of the file as YAML, as in the JSON-RPC editor.

`Ctrl+R` connects. The session runs the file's steps, then stays open until you close it (no headless bounds); while it is open, `Ctrl+R` sends the Compose frame instead of starting another send, and `Esc` closes the session with code 1000, keeping its timeline. A second `Esc` cancels outright. When the session ends, the response viewer shows its record and assertions as for any request, and it is recorded in history. Entries are redacted before they reach the pane: a secret of the request that a server echoes shows as `«redacted:<name>»`.

### The messaging editor

A `kind: kafka`, `kind: mqtt` or `kind: amqp` request (see [Messaging](messaging.md#tui)) opens in the messaging editor. The first row shows `PRODUCE` or `CONSUME` and the broker address. The editor tabs are `Messages`, `Compose`, `Broker` and `Assertions` (`1`–`4` with the tab bar focused).

- **Messages** shows the consumer: `Consuming · 120 messages`, one row per message (where it came from, its key and the start of its value) and, below, the selected message's value: pretty-printed JSON, `decoded from Schema Registry id <n>`, text, or a hex dump for binary values. With the content focused, `↑`/`↓` (or `k`/`j`) select a message, `g` goes to the first and `G` (or `End`) follows the newest, and `p` (or Space) pauses the list (`Paused · 120 messages (8 more since the pause)`). The pane keeps at most 10000 messages; when the buffer is full the consumer stops (`Buffer full`).
- **Compose** holds the messages a produce sends, **Broker** the destination and connection settings, and **Assertions** the assertions, each as YAML.

`Ctrl+R` runs the operation. A consume has no headless bounds: it runs until `Esc` stops it, keeping what it collected; a second `Esc` cancels outright. When it ends, the response viewer shows its record and assertions, and it is recorded in history. Messages are redacted before they reach the pane.

### Variables

Every `{{variable}}` in the URL, params, headers, auth and body is highlighted: in the accent colour when it resolves, in the error colour (and underlined, so it shows without colour) when no layer defines it. When the cursor is on a variable (in the URL, a text body, or the selected table row) a line at the bottom of the editor shows its value and the layer it comes from, e.g. `{{base_url}} = http://localhost:8080 · environment`, or `{{base_url}} is not defined in any layer`. Variables are looked up with the request's own precedence (request, collection, active environment, `rustman.toml`). Secrets are never read for this and show masked, `{{token}} = •••••••• · secret (environment)`, unless you [reveal secrets](#environments-and-secrets).

### External editor

`Ctrl+E` (editor focused, raw body) writes the body to a temp file named for its type (`.json`, `.xml`, `.txt`), hands the terminal to `$VISUAL`, else `$EDITOR`, else `vi` (`notepad` on Windows), and waits. When the editor exits the TUI comes back with the edited body (the tab is marked dirty if it changed) and the temp file is deleted. Values with arguments such as `EDITOR="code --wait"` work. On Unix the value is split like a shell would. On Windows a value that names an existing file (quoted or not) is used as is, so `EDITOR=C:\Windows\System32\notepad.exe` and paths with spaces work; anything else is split with the Windows command-line rules (backslashes are literal, double quotes group). If the configured editor cannot be started, `vi` (`notepad` on Windows) opens instead and a warning toast says why.

### Sending and cancelling

`Ctrl+Enter` sends the current tab's request (`Ctrl+R` does too, for terminals that cannot encode `Ctrl+Enter`). The request is sent in the background with the active environment, through the same engine as `rustman send`: variables are resolved the same way, cookies, captures and OAuth2 tokens are shared with the CLI, and the exchange is recorded in history (redacted), unless `[history] enabled = false`. Unsaved edits are sent as they are; the file is not saved. A new tab that was never saved is sent and recorded as `untitled.rman.yaml`, and in a scratch session nothing is recorded.

While the request is in flight the response viewer shows a spinner and the elapsed time (`⠋ Sending… 1.2 s`), and the status bar counts the requests in flight. Everything else keeps working: other tabs, the sidebar, editing, even sending from another tab. `Esc` or `Ctrl+C` cancels the current tab's request: the connection is closed at once and the viewer shows `Cancelled after 3.0 s`. Closing the tab or quitting cancels it too, and sending again from the same tab replaces the request in flight. `Ctrl+C` with nothing in flight never quits; it shows `Press Ctrl+Q to quit`.

### Copy as code

`Alt+C` (or **Copy as…** in the palette) opens a picker of the snippet languages of [`rustman export snippet`](snippets.md), the one you used last first. Type to filter (`py req` finds `python-requests`), `↑`/`↓` choose, `Enter` copies the current tab's request, as edited, with the active environment: the toast reads `Copied python-requests snippet`. Each language is also in the palette as **Copy as curl**, **Copy as go** and so on. A `graphql` request is copied as its HTTP POST; other kinds have no snippet and `Alt+C` only shows a toast such as `No snippet export for kind 'kafka'`.

The snippet goes to the clipboard through the OSC 52 escape sequence, which works over SSH and inside tmux and needs no clipboard tool. Rustman uses it in Windows Terminal, iTerm2, WezTerm, kitty, Alacritty, foot, Ghostty, Contour, Rio and VS Code's terminal (`TERM`, `TERM_PROGRAM`, or `LC_TERMINAL`, which SSH forwards), and inside tmux when tmux's `set-clipboard` option is `on` (`set -g set-clipboard on`; tmux's default, `external`, drops the sequence); set `RUSTMAN_OSC52=1` to use it in another terminal, or `RUSTMAN_OSC52=0` to never use it. Without it, or when the sequence would be over 100 KB (base64 makes it about 4/3 of the snippet, so a snippet over about 75 KB), the snippet opens in a read-only pane with the hint `Select to copy`; `↑`/`↓` scroll, `Esc` closes.

Snippets are redacted (`«redacted:token»`) like `export snippet`. While secrets are revealed (`Alt+R`) they hold the secret values instead: the picker shows the row `Secrets revealed: snippet will contain secret values`, and copying takes a second `Enter` (moving the selection or typing asks again). A palette **Copy as** entry opens the picker on its language in that case.

### Importing collections

**Import…** in the palette imports a Postman collection or environment, an Insomnia export, a Bruno collection, a HAR capture or `.http` files into the workspace, with the engine of [`rustman import`](import.md). Type the file or folder (relative to the workspace root, or absolute) and press `Enter`: the format is detected and a dry run shows its preview, the same summary as `rustman import --dry-run`: what would be written, skipped or updated, and which secrets move into the active environment (by name only). `Enter` imports that previewed source; `Esc` before it cancels without writing anything. Once the import runs it cannot be cancelled: the dialog shows `Importing…` until it is done, then the result is reported. The collection goes to the folder named after it, existing files are kept (`--on-conflict skip`), and the sidebar shows the new requests. For `--into`, other conflict policies, HAR filters or the JSON report, use the CLI.

## Environments and secrets

`Alt+E` (or **Switch environment** in the palette) lists the workspace's environments with the active one marked. Below the list it shows the variables the selected environment provides: each name, its layer (`environment`, or `rustman.toml` for workspace variables it does not override) and its value. `Enter` makes the selected environment active for this session: the status bar shows it and the next send resolves its variables from it. The default stays the one in `rustman.toml` (`default_environment`); switching in the TUI does not change any file.

Secret values are shown as `••••••••` in the variable tooltip, the environment switcher and captured values. Response headers or bodies that echo a secret are redacted by the engine before the viewer gets them and show the engine's marker, `«redacted:token»`, the same text history, copies and saved bodies hold. Secret values are not read from the keychain or `.rustman/secrets.yaml` to draw the screen.

`Alt+R` reveals secrets for this session: the status bar shows a `SECRETS VISIBLE` badge in the warning colour, and the tooltip and the environment switcher show the active environment's secret values, read once from the secret store (switching environment reads the new one's). Press `Alt+R` again to mask them; the values are dropped from memory. Nothing about the reveal is saved, so the TUI always starts masked. Exchanges stay redacted while secrets are revealed: history, the response viewer, copies, saved bodies, reports and the viz companion never get secret values.

While certificate verification is off for the active environment's sends (`verify_tls = false` in `rustman.toml`, which every environment uses, or `verify_tls: false` in the settings of the request in the current tab) the status bar shows `TLS: INSECURE` in the error colour, for as long as it stays off, and each exchange sent that way is marked `TLS: INSECURE` in its summary line. Environment files have no TLS setting of their own.

## Response viewer

The response pane starts with a summary line: the status and reason, coloured by class (2xx ok, 3xx accent, 4xx warn, 5xx error, with `✔`/`✖` so it reads without colour), the total time, the body size and the assertion tally, e.g. `✔ 201 Created · 84 ms · 1.2 KiB · 3/3 passed`. An exchange sent with TLS verification off ends with `TLS: INSECURE`. When no response arrived the line names the error category and the engine's message instead (`✖ DNS error · cannot resolve api.test: …`; also `TLS error`, `Timeout`, `Connection error`, `Proxy error`, `Auth error`, `Redirect error`).

Below it are the viewer tabs `Body`, `Headers`, `Timing` and `Assertions` (a GraphQL exchange adds `Data`, `Errors`, `Extensions` and `Events` in front, see [The GraphQL editor](#the-graphql-editor)); with the response focused, `]` and `[` switch between them, and `↑`/`↓`, `PageUp`/`PageDown`, `Home`/`End` (`j`/`k`, `Ctrl+D`/`Ctrl+U`, `gg`/`G` with vim keys) scroll.

Everything the viewer shows is redacted with the exchange's secrets, as in history: a server echoing a token shows `«redacted:token»`, and copying or saving the body copies or saves the redacted text.

- **Body**: JSON is a collapsible tree, parsed once in the background so even large bodies scroll smoothly (only the rows on screen are drawn). `Enter`/`Space` folds or unfolds the selected node, `←`/`→` (or `zc`/`zo`) fold/unfold, `zM`/`zR` fold/unfold everything, and a folded array reads `items: [1000 items]`. `y` copies the selected value (a string without its quotes, anything else as JSON) with the system's clipboard tool (`pbcopy`, `clip`, `wl-copy`, `xclip` or `xsel`), or else through the terminal with an OSC 52 escape sequence, which also works over SSH in terminals that support it. `r` switches between the tree and the pretty-printed text.
  - `f` opens a filter box that takes a [JSONPath](https://www.rfc-editor.org/rfc/rfc9535) expression (not jq): `$.items[*].id` shows only the matching nodes, each named by its path, and the match count. An expression that does not parse shows its error inline and keeps the current view; an empty filter (or `Esc` in the box) shows the whole body again.
  - `/` searches keys and values as you type, unfolding the way to the first match; `n`/`N` go to the next/previous match. On large trees (above 20,000 nodes) the search runs in the background and the box shows `searching…` until it finishes; each keystroke cancels the previous search, so typing never waits for it.
  - HTML, XML, YAML, JavaScript and CSS bodies are highlighted according to the `Content-Type` (bodies above 512 KiB, or with a line longer than 20 KiB such as minified HTML, are shown plain); other text is shown as it is.
  - Binary bodies (images, audio, video, fonts, PDFs, `application/octet-stream`, or anything that is not UTF-8) show a hex dump of the first 4 KiB and the total size. `s` saves the body to a file (relative paths are taken from the workspace root); an existing file is never overwritten.
  - Bodies above the engine's `max_body_bytes` (10 MiB by default, see [Request files](request-files.md)) are kept on disk instead of in memory. Above 50 MiB the tree is disabled: the viewer says so and shows the raw text a page at a time (scrolling past the end of a page loads the next one).
- **Headers**: every response header in the order received, duplicates included (two `Set-Cookie` headers are two rows), then the request's captured variables with their new values; secret ones show `••••••••`.
- **Timing**: a waterfall of the phases the engine measures (DNS, connect, TLS, waiting for the first byte, download), each bar placed and sized by its share of the total, with its duration. Phases that did not happen, such as DNS, connect and TLS on a reused connection, show `—`. The response's `Server-Timing` metrics appear as labeled sub-bars under the waiting phase (`db 53.2 ms`), with their description. When the exchange carried [trace context](tracing.md), a `Trace` block shows the trace id, the span id, the backend's own trace id when its `traceresponse` names a different one, and the trace link when one is configured; `Ctrl+O` (`trace.open`) opens the link in the system browser. Below it, a sparkline of the total times of the last 30 exchanges of the same request file from history, the newest highlighted, with the min, median, p95 and max; with fewer than two exchanges it says `not enough history`.
- **Assertions**: each assertion as passed or failed, with the expected and actual values of failures (`✖ failed status == 200 · expected 200 · actual 404`).
- **Snapshot** (only when a [snapshot](snapshots.md#reviewing-in-the-tui) failed, is missing or was just recorded): each snapshot with its file and diff. `a` approves (writes the received snapshot) and `r` rejects (keeps the stored file); `Alt+A` opens a queue over every failing snapshot in the open tabs. Approving is disabled in CI mode (`CI=true`).

`D` diffs the current exchange against an earlier one: a picker lists the exchanges in history, those of the same request first (`Enter` picks, `Esc` closes), and the diff compares the status, the headers and the body. JSON bodies are compared by path with key order ignored, so `{"a":1,"b":2}` against `{"b":3,"a":1}` reports only `$.b: 2 → 3`; other bodies are compared line by line.

### Viz companion and HTML report

`v` (response focused) opens the current exchange in the optional graphical companion, `rustman-viz`, in its response inspector. The companion is looked for as [the viz protocol](viz-protocol.md) defines: the path in `RUSTMAN_VIZ`, then `rustman-viz` on `PATH`, then `rustman-viz` (`rustman-viz.exe`) next to the `rustman` executable. A `RUSTMAN_VIZ` that is not an executable file is an error naming it; nothing else is tried. The TUI starts the companion, performs the `rustman-viz/1` handshake and sends the exchange (already redacted; a body too large for one protocol line goes through a temp file, removed when the companion exits or when the TUI quits), then leaves the companion running on its own; a toast says `Opened in rustman-viz …`.

When the companion cannot be used, a warning toast says why and offers the HTML report instead:

| Toast | Why |
|---|---|
| `Viz companion not found: …` | no companion was found, or `RUSTMAN_VIZ` names a file that is not executable |
| `Viz companion speaks rustman-viz/2, rustman needs rustman-viz/1; upgrade rustman-viz` | the companion rejected the protocol version |
| `Graphics unavailable (Vulkan)` | the companion could not start its graphics (exit code 3) |
| `Viz companion failed: …` | it could not be started, did not answer within 10 s, or answered something else |

Each ends with `Export HTML report: H`. `H` (response focused, or **Export HTML report** in the palette) writes the current exchange as a self-contained HTML page (summary, assertions, response headers and body, all redacted) to `.rustman/reports/<request>-<time>.html` in the workspace, which `rustman init` keeps out of Git, or to the temp directory in a scratch session, and the toast gives the path. The run dashboard (`v` in a run view) arrives with the TUI's run view.

### MCP inspector

`Alt+I` (or **MCP inspector** in the palette) shows the [MCP inspector](mcp-client.md#the-tui-mcp-inspector) in place of the request and response panes; `Esc` or `Alt+I` hides it again, and the session keeps running while it is hidden. It lists the workspace's MCP servers with their connection status, the connected server's info and capabilities, and the `Tools`, `Resources`, `Prompts`, `Notifications` and `Stderr` tabs, with argument forms, the result view and the dialogs for stdio trust, sampling and elicitation. While a sampling or elicitation request waits for you, the status bar shows `MCP APPROVAL (<n>)` on every screen until it is handled.

### Webhook pane

`Alt+W` (or **Webhook inspector** in the palette) shows the [webhook inspector](webhooks.md) in place of the request and response panes; `Esc` or `Alt+W` hides it again, and the listener keeps running while it is hidden. The pane shows the listener's state and address with the number of buffered captures and their size, the captures newest first (time, method, path, body size and the signature verdict as a coloured badge: `valid`, `invalid`, `missing`, `expired`), the selected capture (request line, receive time, every header in order, then the body), and the event log. Values are redacted with the signature verifier secrets; credentials such as `Authorization` show as placeholders.

| Key | Action |
|-----|--------|
| `s` | Start or stop the listener. It binds `[webhook] host` and `port` from `rustman.toml` (default `127.0.0.1:9090`) and answers with the workspace's `webhook/mocks.yaml`. An address reachable from the network asks for confirmation first (Cancel is preselected); while such a listener runs, the pane shows `REACHABLE FROM THE NETWORK` and the status bar a red badge on every screen. |
| `↑` `↓` (`j` `k`), `Home` `End` (`g` `G`) | Select a capture. |
| `PageUp` `PageDown` | Scroll the selected capture's headers and body a page at a time; the title shows which lines are on screen (`41-80 of 655366`). The body is prepared once per capture and view, and only the lines on screen are drawn, so even a 10 MiB body scrolls smoothly. |
| `v` | Cycle the body view: auto (JSON pretty-printed, form fields, or text), text, JSON, hex. A body that is not UTF-8 is always shown as hex. |
| `r` | Replay the capture to a URL (asked for; the last one is offered again) through the HTTP engine, with the [header hygiene](webhooks.md#replay-and-save) of replay. A toast gives the status. |
| `w` | Save the capture as a `kind: http` request file: asks for the target URL (`{{base_url}}<path>` is offered) and the file name (`webhook-<id>.rman.yaml`). |
| `h` | Save the capture to the workspace history as an inbound entry. |
| `y` | Copy the capture as a curl command sent to the URL it was received on (its `Host`). |
| `c` | Clear the capture buffer. |
| `m` | Edit `webhook/mocks.yaml` (a commented template when it does not exist yet). `Ctrl+S` checks and saves it: a file that does not parse is not written and the error is shown under the text; a running listener reloads the saved mocks. `Esc` closes the editor without saving. |

`?` opens the help; other letters do nothing while the pane is shown. Quitting the TUI stops the listener.

### Recorder pane

`Alt+P` (or **Traffic recorder** in the palette) shows the [traffic recorder](record.md) pane: start and stop the recording proxy, follow its captures live, filter and mark them, inspect one (redacted), and save the marked captures as request files or export them as HAR. The keys and the `CA TRUSTED` status badge are described in [The TUI recorder pane](record.md#the-tui-recorder-pane).

### Mock server pane

`Alt+O` (or **Mock server** in the palette) shows the [mock server](mock.md) in place of the request and response panes; `Esc` or `Alt+O` hides it again, and the server keeps running while it is hidden. The pane shows the server's state and address, and the live request log: time, method, status, the [response source](mock.md#request-log) (`example`, `synthesized`, `prefer`, `override`, `stateful`, `proxy`, `cors` or `error`), the path, the operation or override that answered, and the first validation error. Reload notices appear in the log too. Entries are redacted by the server; credentials such as `Authorization` show as placeholders.

| Key | Action |
|-----|--------|
| `s` | Start or stop the server. Starting asks for the OpenAPI document (a file, relative to the workspace root, or a URL; the last one is offered again). The server binds `[mock] host` and `port` from `rustman.toml` (default `127.0.0.1:4010`), answers with the overrides of the mocks directory and reloads both on change. An address reachable from the network asks for confirmation first (Cancel is preselected). |
| `c` | Clear the log. |

`?` opens the help; other letters do nothing while the pane is shown. Quitting the TUI stops the server.

### Monitors view

**Monitors** in the palette (`monitors.open`) lists the workspace's [monitors](monitors.md#the-tui-monitors-view): last outcome, 24-hour uptime, a latency sparkline and whether a rule is firing, with the selected monitor's firing rules and latest results. `j`/`k` select, `r` runs the selected monitor now in the background (through the monitor policy, the store and its alerts; the list refreshes when it finishes), `v` opens the monitors' trends in the viz companion (or says why it cannot), and `Esc` closes.

## Mouse

When the terminal reports mouse events, clicking focuses a pane, an editor field or a pane switcher label, selects a sidebar row, a request tab or an editor tab; the wheel scrolls the pane under the pointer; and dragging the sidebar's right border resizes it. Set `mouse = false` in `tui.toml` to leave the mouse to the terminal, so its own text selection works; the TUI then doesn't capture mouse events at all.

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

While a text input has the focus (the URL, a text body, a table cell being edited, the sidebar filter), typed characters are text: `?`, `v` and other plain keys type themselves instead of running their action. Keys with `Ctrl` or `Alt`, function keys, `Tab` and `Esc` still work.

`Ctrl+C` never quits. With a request in flight it cancels it; otherwise it shows `Press Ctrl+Q to quit`.

### Command palette

`Ctrl+P` opens a fuzzy finder over every action (with its current keys on the right), every request file and every environment. Type to filter: the letters only have to appear in order, so `swenv` finds **Switch environment**. `↑`/`↓` (or `Ctrl+P`/`Ctrl+N`) choose, `Enter` runs, `Esc` closes. With nothing typed, the entries you used most recently in this session come first.

Choosing an action runs it. Choosing a request file opens it. Choosing an environment makes it the active one for this session; the status bar shows it.

**Switch environment** (`Alt+E`) opens the same finder with environments only, the active one marked, and lists what the selected one provides (see [Environments and secrets](#environments-and-secrets)).

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

Your entries win over the vim preset, which wins over the defaults. Adding a key keeps the action's other keys; remove one with `"none"`. Help and the palette show your keys first, and on-screen hints (`No response yet · press F5 to send`, the quit key in the status bar) name your first key, or drop the advice when an action has no key. The keys of the editor's tables (`a` add, `x` remove, `Space` toggle) are fixed.

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
| Next tab | `tab.next` | `Ctrl+PageDown` |  |
| Previous tab | `tab.prev` | `Ctrl+PageUp` |  |
| Go to tab 1 | `tab.goto_1` | `Alt+1` |  |
| Go to tab 2 | `tab.goto_2` | `Alt+2` |  |
| Go to tab 3 | `tab.goto_3` | `Alt+3` |  |
| Go to tab 4 | `tab.goto_4` | `Alt+4` |  |
| Go to tab 5 | `tab.goto_5` | `Alt+5` |  |
| Go to tab 6 | `tab.goto_6` | `Alt+6` |  |
| Go to tab 7 | `tab.goto_7` | `Alt+7` |  |
| Go to tab 8 | `tab.goto_8` | `Alt+8` |  |
| Go to tab 9 | `tab.goto_9` | `Alt+9` |  |
| Command palette | `palette.open` | `Ctrl+P` |  |
| Help | `help.open` | `?`, `F1` |  |
| Toggle sidebar | `sidebar.toggle` | `Ctrl+B` |  |
| Widen sidebar | `sidebar.grow` | `Alt+Right` |  |
| Narrow sidebar | `sidebar.shrink` | `Alt+Left` |  |
| Open request | `sidebar.open` | `Enter` (sidebar) |  |
| Filter requests | `sidebar.filter` | `/` (sidebar) |  |
| New request file | `sidebar.new` | `n` (sidebar) |  |
| Rename request or folder | `sidebar.rename` | `r` (sidebar) |  |
| Delete request or folder | `sidebar.delete` | `d` (sidebar) |  |
| Collapse folder | `tree.collapse` | `Left` (sidebar) |  |
| Expand folder | `tree.expand` | `Right` (sidebar) |  |
| Focus next | `focus.next` | `Tab` |  |
| Focus previous | `focus.prev` | `Shift+Tab` |  |
| Focus pane to the left | `pane.left` |  | `h` (response) |
| Focus pane to the right | `pane.right` |  | `l` (sidebar) |
| Close or dismiss | `ui.escape` | `Esc` |  |
| Switch environment | `env.switch` | `Alt+E` |  |
| Reveal secrets | `secrets.reveal` | `Alt+R` |  |
| Refresh secrets | `secrets.refresh` |  |  |
| Edit in external editor | `editor.external` | `Ctrl+E` (editor) |  |
| Change body mode or auth type | `editor.mode` | `Alt+M` (editor) |  |
| Show completions | `editor.complete` | `Ctrl+Space` (editor) |  |
| Open in viz companion | `viz.open` | `v` (response) |  |
| Show notification log | `notifications.show` |  |  |
| Move up | `list.up` | `Up` (sidebar, response) | `k` (sidebar, response) |
| Move down | `list.down` | `Down` (sidebar, response) | `j` (sidebar, response) |
| Go to top | `list.top` | `Home` (sidebar, response) | `gg` (sidebar, response) |
| Go to bottom | `list.bottom` | `End` (sidebar, response) | `G` (sidebar, response) |
| Page up | `list.page_up` | `PageUp` (sidebar, response) | `Ctrl+U` (sidebar, response) |
| Page down | `list.page_down` | `PageDown` (sidebar, response) | `Ctrl+D` (sidebar, response) |
| Next response tab | `response.next_tab` | `]` (response) |  |
| Previous response tab | `response.prev_tab` | `[` (response) |  |
| Fold or unfold node | `json.toggle` | `Enter` (response), `Space` (response) |  |
| Fold node | `json.collapse` | `Left` (response), `zc` (response) |  |
| Unfold node | `json.expand` | `Right` (response), `zo` (response) |  |
| Fold all | `json.collapse_all` | `zM` (response) |  |
| Unfold all | `json.expand_all` | `zR` (response) |  |
| Copy value | `json.copy` | `y` (response) |  |
| Toggle raw body | `response.raw` | `r` (response) |  |
| Cycle gRPC timeline direction | `response.timeline_direction` | `d` (response) |  |
| Filter body (JSONPath) | `response.filter` | `f` (response) |  |
| Search body | `response.search` | `/` (response) |  |
| Next match | `search.next` | `n` (response) |  |
| Previous match | `search.prev` | `N` (response) |  |
| Diff with an earlier exchange | `response.diff` | `D` (response) |  |
| Save body… | `response.save_body` | `s` (response) |  |
| Export HTML report | `report.html` | `H` (response) |  |
| Webhook inspector | `webhook.open` | `Alt+W` |  |
| Mock server | `mock.open` | `Alt+O` |  |
| MCP inspector | `mcp.open` | `Alt+I` |  |
| Traffic recorder | `recorder.open` | `Alt+P` |  |
| Refresh GraphQL schema | `graphql.refresh_schema` | `Ctrl+G` |  |
| Start GraphQL subscription | `graphql.subscription_start` | `Alt+S` |  |
| Stop GraphQL subscription | `graphql.subscription_stop` | `Alt+X` |  |
| Send following subgraph pagination | `graphql.paginate` | `Alt+G` |  |
| Cycle blockchain scalar display | `graphql.display` | `u` (response) |  |
| Open result value in EVM toolkit | `graphql.links` | `L` (response) |  |
| Copy as… | `request.copy_as` | `Alt+C` |  |
| Import… | `import.open` |  |  |
| Review snapshots | `snapshot.review` | `Alt+A` |  |
| Monitors | `monitors.open` |  |  |
| Open trace link | `trace.open` | `Ctrl+O` |  |
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

The UI follows an Elm-style loop (design D2 of the change): key presses, mouse events, resizes, clock ticks and finished background work all become actions on one channel; a pure `update` function changes the state and returns effects (load or create a workspace, save, create, rename or delete files, re-read a changed file, run the external editor, send a request, filter or page a body, read history, copy or save, read secrets to reveal them, hand an exchange to the viz companion, write an HTML report); an effect runner task executes those off the render loop and reports back with new actions; `view` draws the state. Nothing on the render loop waits for the disk or the network. A send runs on a thread of its own with the engine's runner and its own connection pool; cancelling drops the request, which closes its connection.

## Screenshots

To render one frame without a terminal, for example for the README:

```text
cargo run -p rustman-tui --example screenshot -- [--ansi] [--size WxH] [--theme NAME] [--ascii] [--no-color] [--open REQUEST] [--response BODY.json] [--keys KEYS] [PATH]
```

It opens PATH as `rustman tui` would, runs the start-up work synchronously, and prints a 120x36 frame (or `--size`) as plain text, or with ANSI colours with `--ansi` (view it with `cat` or `less -R`). It ignores `tui.toml` and renders in 24-bit colour unless `--no-color` is given, so the output depends only on the flags and the workspace.

The README images in `docs/images/tui-*.svg` are real frames rendered off-screen by the `screenshot` example. [`crates/rustman-tui/examples/screenshots/`](../crates/rustman-tui/examples/screenshots/) holds the sample workspace (HTTP, GraphQL, JSON-RPC, `evm-call`, gRPC and Kafka requests, a `.proto` file, an OpenRPC catalog, MCP server definitions and two monitors) and the sample responses, and its `render.sh` renders every frame and converts it to SVG with `ansi2svg.py` next to it. The Monitors view reads its results relative to the current time, so `monitor-store.py` writes 24 hours of sample results into the copied workspace's store just before that frame (a fixed seed keeps their shape; only the timestamps change between renders):

```bash
crates/rustman-tui/examples/screenshots/render.sh
```

| Frame | What it shows |
|---|---|
| `tui-shell` | a request's Headers tab and its JSON response tree |
| `tui-assertions` | failed assertions with expected and actual values |
| `tui-timing` | the timing waterfall and latency sparkline, with a masked bearer token |
| `tui-environments` | the environment switcher with each variable's layer, the secret masked |
| `tui-palette` | the command palette fuzzy-matching `swenv` |
| `tui-help` | the help overlay |
| `tui-graphql` | the GraphQL editor's inline validation and the response's Errors tab |
| `tui-graphql-schema` | the GraphQL schema browser, with a deprecated field |
| `tui-jsonrpc` | JSON-RPC method completion from an OpenRPC catalog |
| `tui-evm` | the EVM pane's Function tab: the view functions of the built-in ERC-20 ABI, before anything is sent |
| `tui-grpc` | the gRPC method browser and a server-streaming call's messages |
| `tui-grpc-timeline` | a gRPC request message and the call's timeline |
| `tui-mcp` | the MCP inspector asking for workspace trust before running a stdio server |
| `tui-messaging` | the messaging editor's Compose tab for a Kafka produce request |
| `tui-monitors` | the Monitors view: two monitors' last outcome, uptime and latency sparkline, a firing rule and the recent runs |

The animated GIF at the top of the README (`docs/images/demo.gif`) is a real session recorded with [VHS](https://github.com/charmbracelet/vhs): [`examples/demo/`](../examples/demo/) holds the todo backend it talks to, its workspace, the `demo.tape` script and a `render.sh` that builds Rustman, starts the backend and records the tape.

To render a single frame:

```bash
cargo run -p rustman-tui --example screenshot -- --ansi --size 136x30 \
    [--open pets/get-pet.rman.yaml --response pet.json] [--keys "Ctrl+P s w e n v"] <workspace> > frame.ans
```

`--open` opens a request file (relative to the workspace root) in a tab, and its background loads (a GraphQL or gRPC schema, an OpenRPC catalog) run before the frame is captured. `--response` shows a JSON file as its exchange, so a frame with a response needs no network and is the same on every run: for an HTTP request it is the body of a `200 OK` with fixed timings and history, and the request's assertions are evaluated against it, so the Assertions tab and the summary line's tally are real results; for a GraphQL request the body is read as a GraphQL result (`Data`, `Errors` and `Extensions` tabs); for a gRPC request the file is the call's record, the `grpc` member of a `rustman.report/v1` request. `--keys` presses space-separated chords before the frame is captured (for example `Ctrl+P` for the palette, `F1` for help, `Alt+I` for the MCP inspector, `Tab 2 Tab` to show and focus the Headers tab, or `Tab Tab Tab ] ]` to focus the response and show its third tab; the URL has the focus first, so a printable key such as `?` is typed into it). Frames that need a running listener or session (webhook captures, the mock server's request log, a connected MCP server, the traffic recorder's captures, a live messaging consume) can't be rendered this way. Neither can a sent `evm-call`'s Result tab: `--response` has no form for an EVM call's record.

## Platform checks

**Automated (CI, every pull request).** The TUI smoke test opens a fixture workspace at 80x24, opens a request from the command palette, sends it to an in-process mock server, opens a slow request, sends it, cancels it with `Esc` (the server sees the connection close), and quits with `Ctrl+Q`.

- `cargo test -p rustman-tui --test pty_smoke` runs the real `rustman_tui::run` in a pseudo-terminal on all three OSes: a Unix pty on Linux and macOS, ConPTY on Windows. Raw mode, the alternate screen, the input reader and the effect runner are the real ones. Keys are written to the pty and the screen is read back through a terminal emulator (`vt100`); after `Ctrl+Q` the test checks that the emulated screen is back to the main screen with the cursor shown.
- On Windows, crossterm turns on VT processing (Windows 10 and later) and writes the same escape sequences as on Unix, so ConPTY sees the alternate screen and cursor changes. ConPTY re-renders the child's output rather than passing it through byte for byte, so the exact `ESC[?1049h` / `ESC[?1049l` bytes are only asserted on Linux and macOS; Windows checks the resulting screen. `portable-pty` opens ConPTY with `PSEUDOCONSOLE_INHERIT_CURSOR`, which makes ConPTY ask for the cursor position first; the test answers it.
- `cargo test -p rustman-tui --lib smoke_tests` also runs the same flow on every OS through the real event loop, effect runner, engine and mock server with an in-memory 80x24 terminal, so a ConPTY problem on a runner image is told apart from a TUI regression.

**Manual check matrix.** Pending manual runs: each cell is filled in (`ok`, or a link to an issue) after opening a workspace, sending, browsing a JSON response, the palette, help and `--ascii` in that terminal and colour mode.

| Terminal | Truecolor | 256 colours | `NO_COLOR` |
|---|---|---|---|
| Windows Terminal | pending | pending | pending |
| conhost (classic console) | pending | pending | pending |
| iTerm2 | pending | pending | pending |
| Alacritty | pending | pending | pending |
| kitty | pending | pending | pending |
| tmux (in any of the above) | pending | pending | pending |
| GNOME Terminal | pending | pending | pending |

Force a mode with `COLORTERM=truecolor`, `COLORTERM= TERM=xterm-256color`, or `NO_COLOR=1`.
