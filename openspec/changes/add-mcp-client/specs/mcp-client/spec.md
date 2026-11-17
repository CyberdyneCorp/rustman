# Spec Delta

## Purpose

Lets users connect to remote MCP servers as a client, explore and call their tools, resources and prompts, approve server-initiated requests, and save MCP calls as replayable request files with assertions.

## ADDED Requirements

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
Before spawning a stdio server command for the first time (or after its command, args or env keys change), the TUI SHALL show the full command line and require confirmation; the approval SHALL be recorded in `.rustman/trust.yaml` keyed by a hash of the definition. Headless commands SHALL refuse untrusted definitions unless `--trust` is passed.

#### Scenario: First spawn in TUI
- **GIVEN** a never-approved stdio definition
- **WHEN** the user connects
- **THEN** the exact command line SHALL be shown and nothing SHALL be spawned until the user confirms

#### Scenario: Changed command headless
- **GIVEN** an approved definition whose `args` changed
- **WHEN** `rustman mcp-client tools files` runs without `--trust`
- **THEN** it SHALL exit non-zero with `mcp: server files is not trusted; review it or pass --trust`

### Requirement: Stdio transport
For stdio servers the system SHALL spawn the process with the configured args, env (merged over the parent environment) and cwd (default: workspace root), exchange newline-delimited JSON-RPC over stdin/stdout, and capture stderr into a per-session log viewable by the user. On disconnect the system SHALL close stdin, wait 5 s, then terminate the process.

#### Scenario: Stderr captured
- **GIVEN** a stdio server that writes warnings to stderr
- **WHEN** the session runs
- **THEN** the warnings SHALL appear in the session's stderr log and SHALL NOT corrupt the protocol stream

#### Scenario: Server exits unexpectedly
- **GIVEN** a connected stdio server
- **WHEN** the process exits with code 1
- **THEN** the session SHALL be marked disconnected with the exit code and last 50 stderr lines

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
Every request SHALL carry a `progressToken`; progress notifications SHALL update a progress indicator. Requests SHALL time out after 60 s by default (configurable per server and per request file), sending `notifications/cancelled` on timeout or user cancel, and reporting `mcp: request timed out after 60s` or `cancelled by user`.

#### Scenario: Timeout
- **GIVEN** a tool that never answers
- **WHEN** 60 s elapse
- **THEN** `notifications/cancelled` SHALL be sent and the call SHALL fail with `mcp: request timed out after 60s`

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

### Requirement: TUI MCP inspector pane
The TUI SHALL provide an MCP inspector pane with: server list and connection status, server info/capabilities, tabs `Tools`, `Resources`, `Prompts`, `Notifications`, `Stderr`, the argument form, result view, and approval dialogs for sampling and elicitation. Layout, keymap and theming follow `add-terminal-ui`.

#### Scenario: Pending approval badge
- **GIVEN** a sampling request arrives while the user is on another pane
- **WHEN** the TUI renders
- **THEN** a persistent badge SHALL indicate a pending approval until it is handled
