# mcp-client Specification

## Purpose
Lets users connect to remote MCP servers as a client, explore and call their tools, resources and prompts, approve server-initiated requests, and save MCP calls as replayable request files with assertions.

## Requirements

### Requirement: MCP server definitions
The workspace SHALL define MCP servers by name in `rustman.toml` under `[mcp_servers.<name>]` or in `mcp/servers/<name>.yaml`. A stdio server SHALL have `command` and optional `args`, `env`, `cwd`; an HTTP server SHALL have `url` and optional `headers` and `auth`. All string values SHALL support `{{variable}}` interpolation. A definition with both `command` and `url` SHALL fail to load.

#### Scenario: Stdio server definition
- **GIVEN** `[mcp_servers.files]` with `command = "npx"` and `args = ["-y", "@modelcontextprotocol/server-filesystem", "."]`
- **WHEN** the workspace is loaded
- **THEN** server `files` SHALL be listed with transport `stdio`

#### Scenario: Ambiguous definition
- **GIVEN** a server definition with both `command` and `url`
- **WHEN** the workspace is loaded
- **THEN** loading SHALL fail with `mcp: server <name> must define either command or url, not both`

### Requirement: Stdio command trust
Before spawning a stdio server command for the first time (or after its command, args, env keys or env values change; a secret counts only by its name), the TUI SHALL show the full command line and require confirmation; the approval SHALL be recorded in `.rustman/trust.yaml` keyed by a hash of the definition. Headless commands SHALL refuse untrusted definitions unless `--trust` is passed.

#### Scenario: First spawn in TUI
- **GIVEN** a never-approved stdio definition
- **WHEN** the user connects
- **THEN** the exact command line SHALL be shown and nothing SHALL be spawned until the user confirms

#### Scenario: Changed command headless
- **GIVEN** an approved definition whose `args` changed
- **WHEN** `rustman mcp-client tools files` runs without `--trust`
- **THEN** it SHALL exit non-zero with `mcp: server files is not trusted; review it or pass --trust`

#### Scenario: Changed env value headless
- **GIVEN** an approved definition whose literal `env` value changed
- **WHEN** a headless command connects to it without `--trust`
- **THEN** it SHALL be refused as untrusted, while a rotated secret in `env` SHALL keep the approval

### Requirement: Trust dialog shows the environment
The trust dialog SHALL also show the working directory and each env variable of the definition as `KEY=value`, with substituted secrets and the values of variables named like a secret (`password`, `token`, `secret`, `api_key`) redacted.

#### Scenario: Env values in the trust dialog
- **GIVEN** a never-approved stdio definition with `env = { MODE = "fast", API_TOKEN = "literal-token-value" }`
- **WHEN** the TUI asks for approval
- **THEN** it SHALL show `MODE=fast` and `API_TOKEN=«redacted:API_TOKEN»`, and never `literal-token-value`

### Requirement: Trust approvals sealed per machine
Each approval in `.rustman/trust.yaml` SHALL be an HMAC-SHA256, under a per-machine key, of the workspace's location, the subject and the definition's hash. The key SHALL be 32 random bytes kept outside every workspace (in the user's data directory, `RUSTMAN_DATA_DIR` when set), created on the first approval and readable by the current user only. An approval that does not verify under this key for this workspace SHALL count as not approved, with or without Git.

#### Scenario: Trust file shipped outside Git
- **GIVEN** a workspace without a Git repository (for example a downloaded archive) that contains a `.rustman/trust.yaml` approving its stdio server, written on another machine or for another workspace
- **WHEN** a headless command connects to that server without `--trust`
- **THEN** it SHALL exit non-zero with `mcp: server <name> is not trusted; review it or pass --trust` and nothing SHALL be spawned

### Requirement: Trust file provenance
A trust file that Git tracks or does not ignore, or that is (or sits under) a symbolic link, SHALL be refused when read as well as when written, so a repository cannot ship approvals for its own commands.

#### Scenario: Committed trust file
- **GIVEN** a repository that commits a `.rustman/trust.yaml` approving its own stdio server
- **WHEN** a headless command connects to that server without `--trust`
- **THEN** it SHALL exit non-zero with `refusing .rustman/trust.yaml: Git tracks it or does not ignore it; delete it and approve the servers again` and nothing SHALL be spawned

### Requirement: Stdio transport
For stdio servers the system SHALL spawn the process with the configured args, env (see "Stdio server environment") and cwd (default: workspace root), exchange newline-delimited JSON-RPC over stdin/stdout, and capture stderr into a per-session log viewable by the user. On disconnect the system SHALL close stdin, wait 5 s, then terminate the process together with the processes it started (its process group on Unix, its process tree on Windows).

#### Scenario: Stderr captured
- **GIVEN** a stdio server that writes warnings to stderr
- **WHEN** the session runs
- **THEN** the warnings SHALL appear in the session's stderr log and SHALL NOT corrupt the protocol stream

#### Scenario: Server exits unexpectedly
- **GIVEN** a connected stdio server
- **WHEN** the process exits with code 1
- **THEN** the session SHALL be marked disconnected with the exit code and last 50 stderr lines

### Requirement: Stdio server environment
A stdio server's environment SHALL be a minimal allowlist of the parent environment, each variable only when set: `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `TERM`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TMPDIR`, `TEMP`, `TMP`, plus on Windows the system variables launchers need (see the scenario). The definition's `env` SHALL be applied over it; no other parent variable SHALL reach the process.

#### Scenario: Windows system variables
- **GIVEN** Rustman runs on Windows
- **WHEN** a stdio server is spawned
- **THEN** it SHALL also inherit `SystemRoot`, `SystemDrive`, `windir`, `ComSpec`, `PATHEXT`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `ProgramData`, `ProgramFiles`, `ProgramFiles(x86)`, `HOMEDRIVE`, `HOMEPATH`, `NUMBER_OF_PROCESSORS` and `PROCESSOR_ARCHITECTURE` when set

#### Scenario: Parent environment not inherited
- **GIVEN** Rustman runs with `FAKE_SECRET_TOKEN` set and a stdio definition with `env = { MODE = "x" }`
- **WHEN** the server is spawned
- **THEN** the process SHALL see `MODE` and `PATH`, and SHALL NOT see `FAKE_SECRET_TOKEN`

### Requirement: Stdio message size limit
A stdout message line longer than 16 MiB SHALL end a stdio session, failing the requests in flight with `mcp: server <name>: the server sent a message larger than 16 MiB; the session was closed`.

#### Scenario: Oversized message
- **GIVEN** a connected stdio server that answers with a 17 MiB line
- **WHEN** the answer arrives
- **THEN** the session SHALL end and the request SHALL fail with that error

### Requirement: Streamable HTTP transport with legacy fallback
For HTTP servers the system SHALL use the Streamable HTTP transport, including `Mcp-Session-Id` handling and SSE response streams. If the initial POST returns 4xx other than 401, the system SHALL retry using the legacy HTTP+SSE transport (GET the SSE endpoint, POST to the announced endpoint) and SHALL label the session `legacy SSE`.

#### Scenario: Streamable HTTP session
- **GIVEN** a Streamable HTTP server that returns `Mcp-Session-Id: abc`
- **WHEN** further requests are sent
- **THEN** each SHALL carry `Mcp-Session-Id: abc`

#### Scenario: Legacy server
- **GIVEN** a server that returns 405 to the initialize POST and serves an SSE endpoint
- **WHEN** the user connects
- **THEN** the session SHALL connect via legacy HTTP+SSE and be labeled `legacy SSE`

### Requirement: Initialization and version negotiation
The client SHALL send `initialize` with its latest supported protocol version, client info `rustman/<version>` and its capabilities (`roots`, `sampling`, `elicitation`), then `notifications/initialized`. If the server answers with an older supported version the session SHALL use it; an unsupported version SHALL fail with `mcp: unsupported protocol version <v>`.

#### Scenario: Older server version
- **GIVEN** a server responding with an older supported protocol version
- **WHEN** initialization completes
- **THEN** the session SHALL use that version and display it

#### Scenario: Unsupported version
- **GIVEN** a server responding with an unknown version
- **WHEN** initialization completes
- **THEN** the session SHALL be closed with `mcp: unsupported protocol version <v>`

### Requirement: Server info and capabilities display
After initialization the system SHALL show server name, version, title, negotiated protocol version, `instructions` text, and the declared capabilities (tools, resources incl. subscribe/listChanged, prompts, logging, completions) so the user knows which operations are available. Operations for undeclared capabilities SHALL be disabled.

#### Scenario: No prompts capability
- **GIVEN** a server that does not declare `prompts`
- **WHEN** the session view is shown
- **THEN** prompt operations SHALL be disabled with reason `server does not declare prompts`

### Requirement: Tool listing
The system SHALL list tools via `tools/list`, following `nextCursor` until exhausted (max 100 pages), showing name, title, description, annotations (readOnlyHint, destructiveHint, idempotentHint, openWorldHint), input schema and output schema. On `notifications/tools/list_changed` the list SHALL be refreshed.

#### Scenario: Paginated tools
- **GIVEN** a server returning tools across 3 pages
- **WHEN** tools are listed
- **THEN** all tools from all pages SHALL be shown

#### Scenario: List changed
- **GIVEN** a listed toolset
- **WHEN** the server sends `notifications/tools/list_changed`
- **THEN** the tool list SHALL be refetched and updated

### Requirement: Schema-driven argument form
For a selected tool, the TUI SHALL render an argument form generated from `inputSchema`: typed inputs for string/number/integer/boolean, enum pickers, nested objects, arrays with add/remove, required markers, defaults and descriptions. A raw JSON editor SHALL always be available as an alternative.

#### Scenario: Enum argument
- **GIVEN** a tool with argument `mode` of `enum ["fast","safe"]`
- **WHEN** the form is shown
- **THEN** `mode` SHALL be a picker with exactly `fast` and `safe`

### Requirement: Client-side argument validation
Before `tools/call`, arguments SHALL be validated against `inputSchema`; violations SHALL block the call and list each error with its JSON pointer. The user MAY override with `--no-validate` (CLI) or an explicit confirm (TUI), and the override SHALL be recorded in history.

#### Scenario: Missing required argument
- **GIVEN** a tool requiring `path`
- **WHEN** the user calls it without `path`
- **THEN** the call SHALL be blocked with `/path: required property missing`

### Requirement: Tool call results
The system SHALL show each content item of a `tools/call` result by type (text, image preview metadata, audio metadata, resource link, embedded resource), `structuredContent` as JSON, and mark results with `isError: true` as tool errors distinct from JSON-RPC protocol errors.

#### Scenario: Tool error vs protocol error
- **GIVEN** one call returning `isError: true` and another returning a JSON-RPC error `-32602`
- **WHEN** both are shown
- **THEN** the first SHALL be labeled `tool error` and the second `protocol error -32602`

### Requirement: Output schema validation
When a tool declares `outputSchema`, the system SHALL validate `structuredContent` against it and report violations as warnings in the TUI and as failures of the implicit `output_schema` assertion in `rustman run`. A missing `structuredContent` for a tool with `outputSchema` SHALL be reported as a violation.

#### Scenario: Output does not match
- **GIVEN** a tool whose `outputSchema` requires `count: integer`
- **WHEN** the result has `structuredContent: {"count": "3"}`
- **THEN** a violation `/count: expected integer` SHALL be reported

### Requirement: Resources
The system SHALL support `resources/list` and `resources/templates/list` (paginated), `resources/read` showing text or base64 blob contents with MIME type, URI-template expansion from user-provided parameters, and `resources/subscribe`/`unsubscribe` with `notifications/resources/updated` shown in the notifications stream.

#### Scenario: Read template resource
- **GIVEN** template `file:///{path}`
- **WHEN** the user enters `path=README.md` and reads it
- **THEN** `resources/read` SHALL be sent for `file:///README.md` and its text shown

#### Scenario: Subscribed update
- **GIVEN** a subscription to a resource URI
- **WHEN** the server sends `notifications/resources/updated` for it
- **THEN** the notification SHALL appear and the user SHALL be offered a re-read

### Requirement: Prompts
The system SHALL support `prompts/list` (paginated) and `prompts/get` with the prompt's declared arguments (required markers), showing the returned messages with role and content type.

#### Scenario: Get prompt
- **GIVEN** prompt `review` with required argument `code`
- **WHEN** the user provides `code` and gets it
- **THEN** the returned messages SHALL be shown with their roles

### Requirement: Argument completion
When the server declares `completions`, the forms for prompt arguments and resource-template parameters SHALL request `completion/complete` as the user types (debounced 250 ms) and offer the returned values.

#### Scenario: Completion suggestions
- **GIVEN** a server that completes `language` with `rust`, `ruby`
- **WHEN** the user types `ru` in that argument
- **THEN** `rust` and `ruby` SHALL be suggested

### Requirement: Logging and notifications stream
The system SHALL show every server notification (log messages with level and logger, progress, list changes, resource updates, cancellations) in a timestamped stream, and SHALL let the user set the server log level via `logging/setLevel` when the `logging` capability is declared.

#### Scenario: Set log level
- **GIVEN** a server with `logging` capability
- **WHEN** the user selects level `debug`
- **THEN** `logging/setLevel` with `debug` SHALL be sent and subsequent debug messages SHALL appear in the stream

### Requirement: Progress, timeout and cancellation
Every request SHALL carry a `progressToken`; progress notifications SHALL update a progress indicator. Requests, `initialize` included, SHALL time out after 60 s by default (configurable per server, per request file, and with `--timeout` of `rustman mcp-client`, which also bounds `initialize`), sending `notifications/cancelled` on timeout or user cancel, and reporting `mcp: request timed out after 60s` or `cancelled by user`.

#### Scenario: Timeout
- **GIVEN** a tool that never answers
- **WHEN** 60 s elapse
- **THEN** `notifications/cancelled` SHALL be sent and the call SHALL fail with `mcp: request timed out after 60s`

#### Scenario: Server never initializes
- **GIVEN** a server that never answers `initialize`
- **WHEN** a session is opened (for example by `rustman mcp-client tools <server> --timeout 500ms`)
- **THEN** it SHALL fail with `mcp: request timed out after <timeout>` and a stdio server SHALL be stopped

#### Scenario: User cancel
- **GIVEN** a long-running call showing progress 40%
- **WHEN** the user presses the cancel key
- **THEN** `notifications/cancelled` SHALL be sent and the result SHALL be `cancelled by user`

### Requirement: Sampling requires human approval
A server `sampling/createMessage` request SHALL be shown to the user with the full messages, system prompt, model preferences and max tokens. The user SHALL either reject it or type/edit the response to return; the system SHALL NEVER answer automatically. Headless commands SHALL reject sampling requests with JSON-RPC error `-1` `User rejected sampling request` unless a canned response file is supplied with `--sampling-response`.

#### Scenario: Interactive sampling
- **GIVEN** a server that sends `sampling/createMessage` during a tool call
- **WHEN** the request arrives in the TUI
- **THEN** an approval dialog SHALL show the messages and nothing SHALL be returned until the user acts

#### Scenario: Headless denial
- **GIVEN** `rustman mcp-client call` without `--sampling-response`
- **WHEN** the server sends a sampling request
- **THEN** the client SHALL respond with an error and the CLI SHALL print a warning naming the request

### Requirement: Elicitation requires human approval
A server `elicitation/create` request SHALL be shown with its message and a form generated from `requestedSchema`; the user SHALL choose `accept` (with validated content), `decline` or `cancel`. Headless commands SHALL respond `decline` and print a warning.

#### Scenario: Accept elicitation
- **GIVEN** an elicitation requesting `{ email: string(format=email) }`
- **WHEN** the user enters a valid email and accepts
- **THEN** the response SHALL be `action: accept` with that content

#### Scenario: Headless decline
- **GIVEN** a headless call
- **WHEN** an elicitation arrives
- **THEN** the client SHALL respond `action: decline`

### Requirement: Roots
The client SHALL advertise the `roots` capability and answer `roots/list` with the roots configured on the server definition (`roots = ["{{workspace}}"]` by default, as `file://` URIs), sending `notifications/roots/list_changed` when the configuration changes during a session.

#### Scenario: Default root
- **GIVEN** a server definition without `roots`
- **WHEN** the server sends `roots/list`
- **THEN** the response SHALL contain exactly the workspace root as a `file://` URI

### Requirement: OAuth 2.1 for HTTP servers
When an HTTP server responds 401 with `WWW-Authenticate` pointing to protected-resource metadata, the system SHALL discover the authorization server metadata, use dynamic client registration if supported (else a configured `client_id`), run authorization code + PKCE (S256) via the system browser and a loopback redirect, include the `resource` parameter, and retry the request with the token.

#### Scenario: First authorization
- **GIVEN** an MCP server requiring OAuth and no cached token
- **WHEN** the user connects
- **THEN** the browser SHALL open the authorization URL with `code_challenge_method=S256` and the session SHALL connect after the callback

#### Scenario: No registration and no client_id
- **GIVEN** an authorization server without dynamic registration and no `client_id` configured
- **WHEN** the user connects
- **THEN** connection SHALL fail with `mcp: oauth requires client_id for <server>`

### Requirement: OAuth metadata validation
Before a login, the protected resource metadata's `resource` SHALL be the server URL or a parent of it on the same origin, and the authorization server metadata's `issuer` SHALL be the URL it was looked up for, and the issuer and the authorization, token and registration endpoints SHALL use `https` (`http` only on a loopback host); otherwise the login SHALL fail before the browser opens.

#### Scenario: Resource of another API
- **GIVEN** a server whose protected resource metadata names a `resource` on another origin
- **WHEN** the user connects
- **THEN** connection SHALL fail with `mcp: oauth: the protected resource <resource> does not match the server URL <url>` and no browser SHALL open

#### Scenario: Another issuer
- **GIVEN** authorization server metadata whose `issuer` differs from the URL it was read for
- **WHEN** the user connects
- **THEN** connection SHALL fail with an `mcp: oauth:` error naming both issuers and no browser SHALL open

#### Scenario: Endpoint that is not https
- **GIVEN** authorization server metadata whose `authorization_endpoint` is a `file:` URL or whose `token_endpoint` is plain `http` on a remote host
- **WHEN** the user connects
- **THEN** connection SHALL fail with `mcp: oauth: refusing OAuth URL <url>: it must use https (http only on a loopback host)`, no browser SHALL open and nothing SHALL be sent to the endpoint

### Requirement: OAuth token storage
Access and refresh tokens SHALL be stored only in the OS keychain (fallback: git-ignored `.rustman/secrets.yaml` per `environments`), refreshed automatically before expiry, and SHALL be redacted in history, logs and reports. `rustman mcp-client logout <server>` SHALL delete them.

#### Scenario: Token refresh
- **GIVEN** an expired access token and a valid refresh token
- **WHEN** a request is made
- **THEN** the token SHALL be refreshed without opening the browser

#### Scenario: Redacted in history
- **WHEN** an OAuth-authenticated call is recorded in history
- **THEN** the `Authorization` header SHALL be stored as `«redacted:...»`

### Requirement: MCP request files
A request file with `kind: mcp` SHALL contain `server` (name), `operation` (`tools/call`, `resources/read`, `prompts/get`, `tools/list`, `resources/list`, `prompts/list`), and `params` (e.g. `name` and `arguments` for tools), with `{{variable}}` interpolation in params. The TUI SHALL save the current call as such a file.

#### Scenario: Save a tool call
- **GIVEN** a successful call of tool `search` with arguments `{q: "rust"}`
- **WHEN** the user saves it
- **THEN** a `*.rman.yaml` file with `kind: mcp`, `server`, `operation: tools/call` and those params SHALL be written

### Requirement: MCP assertions and captures
The `assertions` capability SHALL support for mcp requests: `is_error` (bool), `content_text_contains`, `structured` JSONPath checks rooted at `structuredContent`, `output_schema` (implicit when declared), `tool_exists` (for `tools/list`), and latency; captures SHALL read from `structuredContent` or text content.

#### Scenario: Assert tool exists
- **GIVEN** an mcp request with `operation: tools/list` and assertion `tool_exists: search`
- **WHEN** the server does not expose `search`
- **THEN** the assertion SHALL fail listing the available tool names

### Requirement: Session reuse in runs
Within one `rustman run`, all mcp requests targeting the same server SHALL share one session (initialized once) and the session SHALL be shut down at the end of the run. Request files SHALL execute in collection order.

#### Scenario: Shared session
- **GIVEN** a collection with three mcp requests to server `files`
- **WHEN** it runs
- **THEN** exactly one `initialize` SHALL be sent and the stdio process SHALL be spawned once

### Requirement: History for MCP exchanges
Every MCP operation SHALL be recorded by `request-history` with server name, transport, protocol version, method, params (redacted), result or error, duration, and any notifications received during the call (max 200).

#### Scenario: Notifications captured
- **GIVEN** a tool call that emitted 5 progress notifications
- **WHEN** it completes
- **THEN** the history entry SHALL include those 5 notifications

### Requirement: MCP client CLI
`rustman mcp-client tools <server>`, `call <server> <tool> [--args JSON | --arg k=v]`, `resources <server> [--read URI]`, `prompts <server> [--get NAME --arg k=v]`, and `logout <server>` SHALL be provided; `<server>` MAY be a workspace name, `stdio:<command line>` or an `http(s)://` URL. Output SHALL be human-readable by default and JSON with `--json`. Exit code SHALL be 1 for `isError` results and protocol errors.

#### Scenario: Ad-hoc HTTP server
- **WHEN** the user runs `rustman mcp-client tools https://mcp.example.com/mcp --json`
- **THEN** the tool list SHALL be printed as JSON

#### Scenario: Tool error exit code
- **WHEN** `rustman mcp-client call files read --arg path=missing` returns `isError: true`
- **THEN** the content SHALL be printed and the exit code SHALL be 1

### Requirement: MCP client CLI interruption
Ctrl+C during a `rustman mcp-client` command SHALL cancel the request in flight, stop the server together with the processes it started, and exit with 130. On Unix, SIGTERM and SIGHUP SHALL be handled the same way. A second Ctrl+C SHALL quit at once with 130, even during the server's shutdown grace period, and SHALL first kill every stdio server process tree still running.

#### Scenario: Ctrl+C while waiting for a server
- **GIVEN** `rustman mcp-client call` waiting on a stdio server whose launcher started a background process
- **WHEN** the user presses Ctrl+C
- **THEN** the command SHALL exit with 130 and the background process SHALL no longer run

#### Scenario: Second Ctrl+C during shutdown
- **GIVEN** `rustman mcp-client call` on a stdio server that ignores the end of its stdin and whose launcher started a background process
- **WHEN** the user presses Ctrl+C, then again during the 5 s shutdown grace period
- **THEN** the command SHALL exit with 130 at once and the background process SHALL no longer run

#### Scenario: SIGTERM
- **GIVEN** `rustman mcp-client call` waiting on a stdio server whose launcher started a background process
- **WHEN** the process receives SIGTERM
- **THEN** the request SHALL be cancelled, the command SHALL exit with 130 and the background process SHALL no longer run

### Requirement: TUI MCP inspector pane
The TUI SHALL provide an MCP inspector pane with: server list and connection status, server info/capabilities, tabs `Tools`, `Resources`, `Prompts`, `Notifications`, `Stderr`, the argument form, result view, and approval dialogs for sampling and elicitation. Layout, keymap and theming follow `add-terminal-ui`.

#### Scenario: Pending approval badge
- **GIVEN** a sampling request arrives while the user is on another pane
- **WHEN** the TUI renders
- **THEN** a persistent badge SHALL indicate a pending approval until it is handled
