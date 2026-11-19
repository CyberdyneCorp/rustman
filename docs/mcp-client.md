# MCP client

Rustman can connect to other MCP (Model Context Protocol) servers as a client, to explore and test them like any other API. This page covers server definitions, workspace trust, the transports and the session lifecycle, tools, resources, prompts and completion, notifications, progress and cancellation, server-initiated requests, OAuth, `kind: mcp` request files with their assertions, captures, runs and history, `rustman mcp-client`, and the TUI MCP inspector. Rustman as an MCP server for agents is a separate feature, described in [Agents (MCP server)](agent.md).

## Server definitions

MCP servers are defined by name, either in `rustman.toml`:

```toml
[mcp_servers.files]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]
env = { LOG_LEVEL = "info", API_TOKEN = "{{api_token}}" }
cwd = "fixtures"          # optional; default: the workspace root

[mcp_servers.remote]
url = "{{mcp_base}}/mcp"
headers = { X-Team = "core" }
auth = { type = "bearer", token = "{{mcp_token}}" }
timeout_ms = 30000        # optional
```

or one per file in `mcp/servers/<name>.yaml` (the file name is the server name):

```yaml
url: "{{mcp_base}}/mcp"
headers:
  X-Team: core
auth:
  type: bearer
  token: "{{mcp_token}}"
```

| Key | Applies to | Meaning |
| --- | --- | --- |
| `command` | stdio | The program to spawn. |
| `args` | stdio | Its arguments. |
| `env` | stdio | Variables set for the process. The process inherits only a few of Rustman's own variables (see [Transports](#transports)); everything else it needs goes here. |
| `cwd` | stdio | Its working directory, relative to the workspace root (default: the root). |
| `url` | HTTP | The server's MCP endpoint. |
| `headers` | HTTP | Headers sent with every request. |
| `auth` | HTTP | Any [auth helper](http.md) (`bearer`, `basic`, `api_key`, ...). |
| `oauth` | HTTP | A pre-registered OAuth client: `client_id`, `client_secret`, `scopes` (see [OAuth](#oauth)). |
| `roots` | both | The roots answered to the server's `roots/list` (default `["{{workspace}}"]`). |
| `timeout_ms` | both | The request timeout of this server (default 60 000). |

A server has either `command` (a stdio server) or `url` (an HTTP server). A definition with both fails to load with `mcp: server <name> must define either command or url, not both`; one with neither, or with keys of the other transport, fails too. Server names use letters, digits, `-`, `_` and `.`, and a name may be defined only once across `rustman.toml` and `mcp/servers/`. Unknown keys are errors.

Every string value supports `{{variable}}` interpolation from the active environment, its secrets and `[variables]`, resolved when a session is opened. Secret values are redacted from everything a session shows: its stderr log, errors and history.

## Workspace trust

Spawning a stdio server runs a command the workspace chose, so Rustman asks first. Before a stdio server is spawned for the first time, or after its `command`, `args`, `cwd`, `env` keys or literal `env` values change, the full command line is shown and nothing is spawned until you confirm. Headless commands refuse an untrusted server with:

```
mcp: server files is not trusted; review it or pass --trust
```

`--trust` approves the server as it is defined now.

Approvals are kept per machine in `.rustman/trust.yaml` (git-ignored, readable by you only; trust is never shared through Git). An approval covers a SHA-256 fingerprint of the command, args, env (sorted by key) and cwd. A secret substituted into any field counts only by its name, so rotating a secret never asks again, while adding an env variable, changing a literal env value (such as `PATH` or `NODE_OPTIONS`, which decide what code runs) or changing an argument does. HTTP servers run nothing locally and need no approval.

Each entry is sealed with an HMAC-SHA256 under a key that never leaves this machine: 32 random bytes in `trust.key` in your data directory (`$RUSTMAN_DATA_DIR`, else `~/Library/Application Support/rustman` on macOS, `$XDG_DATA_HOME/rustman` or `~/.local/share/rustman` on Linux, `%APPDATA%\rustman` on Windows), created with the first approval and readable by you only. The seal also covers the workspace's location. A trust file that came with a workspace (committed, or inside a downloaded archive with no Git at all), copied from another machine or from another workspace therefore approves nothing, and the server is asked about (or refused headless) as if it were new. Moving a workspace folder, or deleting `trust.key`, asks again.

As a second guard, a trust file that Git tracks or does not ignore, or that is a symbolic link, is refused rather than read:

```
refusing .rustman/trust.yaml: Git tracks it or does not ignore it; delete it and approve the servers again
```

`.rustman/trust.yaml` is a small library store other features can reuse: each entry maps a subject (here `mcp/<name>`) to the seal of the approved fingerprint.

## Transports

**stdio.** The server is spawned with its args and cwd, and speaks newline-delimited JSON-RPC over stdin and stdout. Its environment is not Rustman's: it gets only `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `TERM`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TMPDIR`, `TEMP` and `TMP` (on Windows also `SystemRoot`, `SystemDrive`, `windir`, `ComSpec`, `PATHEXT`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `ProgramData`, `ProgramFiles`, `ProgramFiles(x86)`, `HOMEDRIVE`, `HOMEPATH`, `NUMBER_OF_PROCESSORS` and `PROCESSOR_ARCHITECTURE`) when they are set, plus the definition's `env` over them. Tokens and credentials in your shell (`GITHUB_TOKEN`, `AWS_*`...) never reach a server unless its definition passes them, for example `env = { GITHUB_TOKEN = "{{github_token}}" }`. Everything it writes to stderr goes to the session's stderr log (secrets redacted), never into the protocol stream. If the process exits on its own, the session is marked disconnected with the exit code and the last 50 stderr lines. Disconnecting closes stdin, waits 5 seconds for the process to exit, then kills it together with every process it started, so a server behind a launcher such as `npx`, `uvx` or `sh -c` is not left running (on Unix the server runs in a process group of its own; on Windows its process tree is killed with `taskkill`). On Windows a bare command such as `npx` is looked up with `PATHEXT`, so `npx.cmd` and `.bat` launchers are found. A server that does not answer `initialize` within its timeout (`--timeout` of `rustman mcp-client`, else `timeout_ms`, default 60 s) fails with `mcp: request timed out after 60s`. Each stderr line keeps at most 4 KiB (the rest is replaced by ` [truncated]`), and a message larger than 16 MiB (a stdio line, or an HTTP server's event) ends the session, so a misbehaving server cannot exhaust memory; the requests in flight fail with `mcp: server <name>: the server sent a message larger than 16 MiB; the session was closed`.

**Streamable HTTP.** Requests go through Rustman's HTTP engine, so the workspace TLS settings and proxies apply, plus the definition's `headers` and `auth`. The `Mcp-Session-Id` the server assigns is sent on every following request, responses may be JSON or SSE streams, and the session is deleted on disconnect.

**Legacy HTTP+SSE.** If the server answers the `initialize` POST with a 4xx status other than 401, Rustman retries with the pre-2025 transport: it GETs the same URL as an event stream, waits for the `endpoint` event, and POSTs messages there. Such sessions are labelled `legacy SSE`.

## Initialization

Rustman sends `initialize` with protocol version `2025-11-25`, client info `rustman/<version>` and the `roots`, `sampling` and `elicitation` capabilities, then `notifications/initialized`. A server answering with an older supported version (`2024-11-05`, `2025-03-26`, `2025-06-18`) gets a session in that version; any other version closes the session with `mcp: unsupported protocol version <v>`.

After initialization the session shows the server name, version and title, the negotiated protocol version, the server's `instructions`, and its declared capabilities (tools, resources with subscribe and listChanged, prompts, logging, completions). Operations of capabilities the server does not declare are disabled with a reason such as `server does not declare prompts`.

## Tools

`tools/list` follows `nextCursor` until the list is complete (at most 100 pages; more fails with `mcp: tools/list returned more than 100 pages`) and shows each tool's name, title, description, annotations (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`), input schema and output schema. When the server sends `notifications/tools/list_changed`, a listed toolset is fetched again.

**Argument forms.** A tool's `inputSchema` becomes a form: typed inputs for strings, numbers, integers and booleans, a picker for `enum` (and `const`), nested objects, arrays whose items are added and removed, a variant picker for `oneOf`/`anyOf`, required markers, defaults and descriptions. A subtree using any other keyword (`patternProperties`, `$ref`, ...) is edited as raw JSON, and the raw JSON editor is always available for the whole arguments object.

**Validation.** Arguments are validated against `inputSchema` before `tools/call` is sent (with the same `jsonschema` engine as the [`schema` assertion](assertions.md)), and violations block the call, each named by its JSON pointer:

```
mcp: invalid arguments: /path: required property missing
```

`--no-validate` (CLI) or an explicit confirm (TUI) sends the call anyway; the overridden violations are kept with the result for history.

**Results.** Each content item is shown by type: text, image and audio (type and size), resource links, and embedded resources (their text, or the size of their blob). `structuredContent` is shown as JSON. A result with `isError: true` is labelled `tool error`; a JSON-RPC error is labelled `protocol error <code>` (for example `protocol error -32602`). When the tool declares an `outputSchema`, `structuredContent` is validated against it, and each violation (such as `/count: expected integer`, or `/: structuredContent missing`) is reported as a warning (and, in `rustman run`, a failure of the implicit `output_schema` assertion).

## Resources

`resources/list` and `resources/templates/list` follow every page. `resources/read` shows each content item's URI, MIME type and text, or its base64 blob. Templates are expanded from the parameters you enter (RFC 6570 `{var}`, `{+var}` for reserved characters such as `/`, and `{a,b}` lists); template `file:///{path}` with `path=README.md` reads `file:///README.md`. When the server declares `resources.subscribe`, you can subscribe to a resource: its `notifications/resources/updated` appear in the notifications stream with an offer to read it again.

## Prompts and completion

`prompts/list` follows every page and shows each prompt's arguments with required markers; `prompts/get` checks required arguments first (`mcp: invalid arguments: /code: required property missing`) and shows the returned messages with their roles and content types. When the server declares `completions`, the forms of prompt arguments and resource-template parameters ask `completion/complete` as you type, 250 ms after the last keystroke, and offer the returned values.

## Notifications, progress and cancellation

Every server notification of a session (log messages with level and logger, progress, list changes, resource updates, cancellations) is kept in a timestamped stream, with secrets redacted. When the server declares `logging`, you can set its log level (`logging/setLevel`); messages below the level are no longer sent.

Every request carries a `progressToken`, and progress notifications update the request's progress indicator. The notifications received while a request runs are kept with its result (at most 200). A request times out after 60 seconds by default; `timeout_ms` on the server definition, or on a request file, changes that. On timeout Rustman sends `notifications/cancelled` and the request fails with `mcp: request timed out after 60s`; cancelling it yourself sends the same notification and ends it with `cancelled by user`.

## Server-initiated requests

A server may ask the client to sample an LLM (`sampling/createMessage`) or to ask you something (`elicitation/create`). Rustman never answers either on your behalf:

- **Sampling.** The TUI shows the full messages, system prompt, model preferences and max tokens, and nothing is returned until you reject the request or type (or edit) the response. A pending request shows a badge until it is handled. Headless commands reject it with JSON-RPC error `-1` `User rejected sampling request` and print a warning naming the request, unless `--sampling-response <file>` gives a canned answer: a JSON `CreateMessageResult` such as `{"role": "assistant", "content": {"type": "text", "text": "..."}, "model": "human"}`.
- **Elicitation.** The TUI shows the server's message and a form generated from `requestedSchema`; you choose `accept` (with content that passes the schema, formats such as `email` included), `decline` or `cancel`. Headless commands decline and print a warning.

There is no setting that approves these requests automatically.

**Roots.** Rustman declares the `roots` capability and answers `roots/list` with the definition's `roots` as `file://` URIs: `{{workspace}}` is the workspace root, relative paths are taken from it, and `file://` URIs are kept. Without `roots`, the answer is exactly the workspace root. Changing the roots during a session sends `notifications/roots/list_changed`.

## OAuth

When an HTTP server without `auth` answers `401`, Rustman logs in with OAuth 2.1:

1. It reads the protected resource metadata (RFC 9728) named by the `resource_metadata` parameter of the `WWW-Authenticate` challenge (or at `/.well-known/oauth-protected-resource`), then the metadata of its authorization server (RFC 8414, with the OpenID Connect discovery URL as fallback). The advertised `resource` must be the server URL or a parent of it on the same origin, and the authorization server's `issuer` the URL it was looked up for; otherwise the login stops before the browser opens (`mcp: oauth: the protected resource <resource> does not match the server URL <url>`), so a server cannot obtain a token meant for another API. The issuer and the authorization, token and registration endpoints must use `https` (`http` only on a loopback host such as `127.0.0.1` or `localhost`); any other URL stops the login before anything is opened or sent (`mcp: oauth: refusing OAuth URL <url>: it must use https (http only on a loopback host)`), so a server cannot have your browser handler open a `file:` or custom-scheme URL, or have a code or client secret sent in plain text.
2. It registers itself as a public client when the authorization server offers dynamic client registration (RFC 7591); otherwise it uses the definition's `oauth.client_id`. With neither, the connection fails with `mcp: oauth requires client_id for <server>`.
3. It opens the authorization URL in your browser (and prints it) with PKCE (`code_challenge_method=S256`), the `resource` parameter naming the MCP server (RFC 8707) and the scopes (`oauth.scopes`, else the ones the resource advertises), and waits for the redirect on a loopback listener (`http://127.0.0.1:<port>/callback`). When the browser cannot reach it (over SSH, say), paste the URL the browser was redirected to.
4. It exchanges the code for tokens and retries the connection.

```toml
[mcp_servers.remote]
url = "https://mcp.example.com/mcp"
oauth = { client_id = "rustman-cli", scopes = ["mcp:read"] }
```

Tokens are stored only in the OS keychain (or, without one, in the git-ignored `.rustman/secrets.yaml`), under `mcp/<hash of the server name and URL>`, together with the token endpoint and client, so an expired access token is refreshed with its refresh token without opening the browser. A failed refresh drops the tokens and the next `401` starts a new login. Token values (and a client secret) are redacted from everything the session shows, and the `Authorization` header is always shown as `«redacted:Authorization»`. `rustman mcp-client logout <server>` deletes them. Headless commands cannot log in: a server that needs a login fails until you connect once from a terminal.

## Request files

A request file with `kind: mcp` saves an MCP call so it can be replayed and asserted on in CI:

```yaml
version: 1
kind: mcp
name: Search for rust
server: files                  # a server of the workspace
operation: tools/call
params:
  name: search
  arguments:
    q: rust
    token: "{{api_token}}"     # string values are interpolated
timeout: 30s                   # optional: replaces the server's timeout
validate: false                # optional: send arguments that break inputSchema
assertions:
  - is_error: false
  - content_text_contains: found
  - structured: {path: $.count, gt: 0}
captures:
  - {var: count, from: json, path: $.count}
```

| Field | Meaning |
| --- | --- |
| `server` | The name of a server the workspace defines. |
| `operation` | `tools/list`, `tools/call`, `resources/list`, `resources/read`, `prompts/list` or `prompts/get`. |
| `params` | `name` and `arguments` for `tools/call` and `prompts/get`, `uri` for `resources/read`. String values support `{{variable}}` interpolation; keys and other values are kept as written. A missing `name` or `uri` fails to load with `mcp: tools/call requires params.name (a string)`. |
| `timeout` | The request timeout (`500ms`, `30s`, `2m`), above the server's `timeout_ms`. |
| `validate` | `false` sends tool arguments that break `inputSchema` (recorded in history). |
| `variables` | Variables of this request, as in `kind: http`. |
| `assertions`, `captures` | See below. |

The TUI saves the current call as such a file (see [the inspector](#the-tui-mcp-inspector)); a saved call is written field by field, and a loaded file keeps its own text.

### Assertions and captures

Besides `latency` and the generic `json`, `body` and `schema` checks (which read the MCP result as JSON), mcp requests have their own assertions:

| Assertion | Passes when |
| --- | --- |
| `is_error: true` / `false` | The tool result has (or has not) `isError: true`. |
| `content_text_contains: <text>` | Some text content item (or embedded text resource) contains the text. |
| `structured: {path, <op>: value}` | A [JSONPath check](assertions.md) rooted at `structuredContent`: `$.count` is `structuredContent.count`. |
| `output_schema: true` | `structuredContent` matches the tool's `outputSchema`. Implicit for every tool that declares one; write `output_schema: false` to expect a violation. |
| `tool_exists: <name>` | A `tools/list` result has the tool. A failure lists the available tools: `tool_exists: expected a tool named "search", got available tools: read, write`. |

A [`snapshot`](snapshots.md) assertion compares the MCP result (the JSON the server answered, with no status or headers) with a stored snapshot, recorded under `__snapshots__/` like those of HTTP requests.

Expected values support `{{variable}}` interpolation. Captures read `structuredContent` when the result has it, else the text content: `from: json` with `$.count` reads `structuredContent.count` (or the text, when it is JSON), and `from: body_regex` reads the text. For `resources/read` they read the text contents; for the list operations, the result JSON.

### In `rustman run`

`rustman run` executes `kind: mcp` files in collection order with the other kinds. All mcp requests of a run that target the same server share one session: it is initialized once (a stdio server is spawned once) and shut down at the end of the run. Runs are headless: an untrusted stdio server makes its requests error with `mcp: server <name> is not trusted; review it or pass --trust` unless `rustman run --trust` is given, sampling is rejected and elicitation declined (each with a warning on the request), and an OAuth login is only possible from a terminal.

A connection, protocol, validation or timeout error marks the request `errored`; failed assertions mark it `failed`, as for HTTP requests.

Sending a `kind: mcp` file from the TUI workbench (`Ctrl+Enter` on an open file) runs it the same way, with one session for that send: a stdio server must have been approved first (in the [inspector](#the-tui-mcp-inspector), or with `--trust`), and OAuth uses the tokens already stored. The agent MCP server's execution tools (`send_request`, `run_collection`; see [Agents](agent.md)) do not connect to other MCP servers: they skip `kind: mcp` requests as an unsupported kind, so an agent cannot spawn workspace commands through them.

### History

Every MCP operation of a run is recorded in [request history](history.md). The entry's request is the method (such as `tools/call`), the server's URL (or `stdio:<command line>`), its headers (credentials such as `Authorization` stored as `«redacted:Authorization»`) and the params; its record adds an `mcp` object:

```json
{
  "server": "files",
  "transport": "stdio",
  "protocol_version": "2025-11-25",
  "method": "tools/call",
  "params": {"name": "search", "arguments": {"q": "rust", "token": "«redacted:api_token»"}},
  "result": {"content": [{"type": "text", "text": "found 3 files"}], "isError": false},
  "outcome": "ok",
  "duration_ms": 12.5,
  "notification_count": 5,
  "notifications": [{"at": 1763460000000, "type": "progress", "token": "1", "progress": 20.0, "total": null, "message": null}]
}
```

`outcome` is `ok`, `tool error`, `protocol error <code>`, `cancelled` or `error`; `error` holds the message when there is no result. At most 200 notifications are kept (`notification_count` counts them all), and `validation_override` lists input violations sent anyway. Params, results, notifications and errors are redacted with every secret of the run and the session's OAuth tokens. MCP entries cannot be replayed with `rustman history replay`.

## rustman mcp-client

```text
rustman mcp-client tools <server> [--json]
rustman mcp-client call <server> <tool> [--args <json> | --arg k=v | --arg k:=<json>]... [--no-validate]
                                        [--sampling-response <file>] [--json]
rustman mcp-client resources <server> [--read <uri>] [--json]
rustman mcp-client prompts <server> [--get <name> [--arg k=v]...] [--json]
rustman mcp-client logout <server>
```

`<server>` is a server of the workspace, `stdio:<command line>` (split like a shell: `stdio:npx -y @modelcontextprotocol/server-everything`), or an `http://` / `https://` URL. A command line typed after `stdio:` is your own, so it runs without a trust prompt and nothing is recorded; a workspace server needs this machine's trust or `--trust`. Ad-hoc servers work outside a workspace too.

| Flag | Effect |
| --- | --- |
| `--json` | Print the MCP result as JSON instead of text. |
| `--trust` | Approve (and record) an untrusted stdio server of the workspace. |
| `--timeout <duration>` | The request timeout, `initialize` included (default: the server's `timeout_ms`, else 60s). |
| `--env <name>` | The environment whose variables and secrets fill the server definition. |
| `--args <json>` | `call`: the arguments as one JSON object. |
| `--arg k=v` | `call`: a string argument; `--arg k:=<json>` gives any JSON value (`--arg limit:=10`). `prompts --get`: a prompt argument. |
| `--no-validate` | `call`: send arguments that break the tool's `inputSchema`. |
| `--sampling-response <file>` | `call`: answer sampling requests with this `CreateMessageResult` instead of rejecting them. |

The text output lists one tool per line with its annotations (`search [read-only]`) and description, one resource per line (`file:///notes.md  notes  text/markdown`), prompts with their arguments (`review  (code*)`, `*` marks required ones), a tool result's content items and `structuredContent` (with `tool error` first for an `isError` result and `warning: outputSchema: ...` lines for violations), and a prompt's messages as `role: content`.

| Exit code | When |
| --- | --- |
| 0 | The operation succeeded. |
| 1 | The tool returned `isError: true` (its content is printed), or the server answered with a JSON-RPC error (`error: mcp: protocol error -32602: ...`). |
| 2 | A usage, definition or trust problem: an unknown server, invalid arguments, a server that is not trusted. |
| 3 | The server could not be started or reached, failed, or did not answer in time. |
| 130 | Interrupted with `Ctrl+C` (on Unix also `SIGTERM` or `SIGHUP`): the request in flight is cancelled (`notifications/cancelled`) and the server is stopped with every process it started, after its 5-second grace period. A second `Ctrl+C` quits at once, still killing the server's processes first. |

```sh
rustman mcp-client tools https://mcp.example.com/mcp --json
rustman mcp-client call files read --arg path=missing     # prints the content, exits 1
rustman mcp-client resources files --read file:///README.md
rustman mcp-client prompts files --get review --arg code='fn main() {}'
```

## The TUI MCP inspector

`Alt+I` (or **MCP inspector** in the command palette) opens the inspector in place of the request and response panes; `Esc` or `Alt+I` hides it, and the session keeps running.

- **Servers.** The left column lists the workspace's servers with their transport and status (`○` idle, `…` connecting, `●` connected, `✗` failed). `Enter` connects; `d` disconnects. A stdio server that is not trusted yet shows its full command line, working directory and env variables (`KEY=value`, the values of secrets and of variables named like one, such as `API_TOKEN`, redacted) first, and nothing runs until you press `y` (which records the approval in `.rustman/trust.yaml`); `n` cancels. An HTTP server that asks for OAuth opens the authorization URL in your browser and shows it in a toast; the session connects once the browser reaches the loopback redirect, and the tokens are kept in the workspace's secret store as for `rustman mcp-client`.
- **Session.** The title shows the server name and version, the transport and the negotiated protocol version; the first line lists the declared capabilities. Tabs `1`-`5` switch between `Tools`, `Resources`, `Prompts`, `Notifications` and `Stderr`; a tab whose capability is not declared shows the reason, such as `server does not declare prompts`. `Tab` moves the focus between the servers, the list and the form.
- **Forms.** `Enter` on a tool opens the form generated from its `inputSchema`: one row per property with its type, a `*` for required ones and the description below. Type into text, number and JSON rows, toggle booleans with `Space`, and pick `enum` values with `←`/`→` (exactly the values the schema lists). `Ctrl+J` swaps the rows for the raw JSON editor and back. `Enter` sends the call; arguments that break the schema are refused with each violation, and `Ctrl+O` sends them anyway (the override is recorded). Prompts open a form of their arguments; `Enter` on a resource reads it, and on a template opens a form of its variables.
- **Results.** The result view shows the call, its label (`ok`, `tool error`, `protocol error <code>`, `cancelled`), each content item, `structuredContent` and `outputSchema` warnings. A call in flight shows its progress in the top line; `Ctrl+C` cancels it (`notifications/cancelled`). `Ctrl+S` saves the last call as a `kind: mcp` request file in the workspace.
- **Notifications and stderr.** The `Notifications` tab is the timestamped stream; `l` cycles the server log level (`debug`, `info`, `warning`, `error`), `s` on a resource subscribes to it, and `r` reads again the resource a `notifications/resources/updated` offered. `Stderr` shows the stdio server's stderr log.
- **Approvals.** A sampling request opens a dialog with its messages, system prompt, model preferences and max tokens: type the response and press `Ctrl+S` to send it, or `Ctrl+R` / `Esc` to reject. An elicitation shows the server's message and a form from its requested schema: `Ctrl+S` accepts (only with content that passes the schema), `Ctrl+D` declines and `Esc` cancels. Until a request is handled, the status bar shows `MCP APPROVAL (<n>)` on every screen.

The reference-server contract test (`crates/rustman-mcp/tests/client_everything.rs`) runs the client against `@modelcontextprotocol/server-everything`; it is ignored by default and runs with `cargo test -p rustman-mcp --test client_everything -- --ignored` when `RUSTMAN_MCP_EVERYTHING` points at its `dist/index.js`. CI installs the package in the `mcp-reference-server` job.
