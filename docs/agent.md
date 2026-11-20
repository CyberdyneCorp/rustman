# Agents (MCP server)

`rustman mcp serve` runs Rustman as a [Model Context Protocol](https://modelcontextprotocol.io) server, so an AI agent (Claude Code, Claude Desktop, IDE agents, any MCP client) can use your workspace: its saved requests, environments, explorations and reports. The agent goes through the same engine and the same [safety policy](safety.md) as you. It can't read secret values, it is read-only unless `rustman.toml` says otherwise, and every call it makes is written to an audit log.

The server is built on the official Rust SDK, `rmcp` 3.10. It negotiates MCP protocol revisions up to `2025-11-25` through `initialize`.

## Setup

### Claude Code

From the workspace folder (or any folder below it):

```sh
claude mcp add rustman -- rustman mcp serve
```

### Claude Desktop and other clients

Configure a stdio server whose command is `rustman` with the arguments `mcp serve --workspace /path/to/workspace`:

```json
{
  "mcpServers": {
    "rustman": {
      "command": "rustman",
      "args": ["mcp", "serve", "--workspace", "/path/to/workspace"]
    }
  }
}
```

`rustman mcp serve` finds the workspace like every other command: `--workspace <dir>`, else the folder holding `rustman.toml` at or above the current one. Without a workspace it exits with code 2 before the handshake. The session starts in the active environment (`--env`, else `RUSTMAN_ENV`, else `default_environment`); an agent can switch environments for its own session only.

Over stdio, stdout carries only MCP messages. Warnings, errors and logs (`-v`, `RUST_LOG`) go to stderr, redacted.

## HTTP mode

For clients that can't spawn a process, `--http` serves the Streamable HTTP transport at `/mcp`:

```sh
rustman mcp serve --http                    # http://127.0.0.1:8765/mcp
rustman mcp serve --http 127.0.0.1:9000 --token-file ~/.config/rustman/mcp-token
```

HTTP mode is a convenience for local tools, not a hosted service. Three locks protect it:

- **Loopback bind.** The default address is `127.0.0.1:8765`. A non-loopback address such as `0.0.0.0:8765` is refused with exit code 2 unless `--allow-remote` is also given, and then a warning is printed. Requests whose `Host` is not a loopback name are rejected. When `--allow-remote` serves a non-loopback address this `Host` check is off (on a loopback address it stays on), because remote clients reach the server under names it cannot know: the bearer token and the Origin check below are then the only locks, so keep the token secret and serve remote mode only on a trusted network.
- **Bearer token.** Every request needs `Authorization: Bearer <token>`. The token is read from `--token-file` (surrounding whitespace is ignored, and an empty file is an error) or generated at startup (32 random bytes, base64url) and printed once on stderr. A missing or wrong token gets `401` and never reaches MCP. Tokens are compared in constant time.
- **Origin check.** A request carrying an `Origin` header gets `403` unless the origin is a loopback origin (`localhost`, `127.0.0.0/8`, `[::1]`) or listed in `[agent].allowed_origins`. This stops a web page from driving the server through DNS rebinding.

There is no TLS and no OAuth: keep HTTP mode on the loopback interface.

## Permissions: the `[agent]` table

Agent permissions come only from `rustman.toml`. No command-line flag raises them, so an agent that can run shell commands can't relaunch the server with more rights, and a change to them shows up in `git diff` and code review.

Without an `[agent]` table, these defaults apply:

```toml
[agent]
allow_mutations = false       # POST, PUT and PATCH
allow_destructive = false     # DELETE and unknown methods (implies allow_mutations)
require_confirmation = true   # mutations need a client that can ask the human
allowed_hosts = []            # empty: the hosts of the session's active environment
allowed_origins = []          # browser origins accepted in HTTP mode, besides loopback
max_output_bytes = 262144     # cap on one serialized tool result (256 KiB)
max_jobs = 2                  # background jobs at once
```

`allowed_hosts` and `allowed_origins` take origins written as `scheme://host[:port]`, as `[safety].allowed_hosts` does. Unknown keys and invalid values are errors, so a misspelled permission is never ignored.

### What an agent may send

Every request an agent triggers, saved or ad hoc, passes these checks before anything is sent:

1. **Method.** By default only GET, HEAD and OPTIONS. `allow_mutations = true` adds POST, PUT and PATCH; `allow_destructive = true` adds DELETE and every other method. A refused method gets `code: "blocked"` with the reason `mutations not allowed for agents` or `destructive not allowed for agents`.
2. **Allowlist.** The origin must be in `[agent].allowed_hosts`, or, when that is empty, among the `http(s)` origins of the session's active environment variables (`base_url: http://127.0.0.1:8080/api` allows `http://127.0.0.1:8080`). `[safety].allowed_hosts` doesn't apply to agents. Anything else gets `code: "blocked"` with the reason `host not allowed`, and so does every redirect hop that leaves the allowlist.
3. **Production.** An environment marked `production: true`, or a host in `[safety].production_hosts`, is refused: agents can't confirm production hosts.
4. **Confirmation.** See below.

All the requests of a session share one rate limit and one concurrency limit, from `[safety].rate` and `[safety].concurrency`, `rpc_call` calls included (over HTTP or WebSocket).

`rpc_call` sends JSON-RPC calls, which are all POSTs: instead of the method rule it only sends methods matching `[jsonrpc] agent_read_only_methods`, and refuses subscriptions; the allowlist and production checks apply as above. See [JSON-RPC](jsonrpc.md#agent-tool).

`grpc_list`, `grpc_describe` and `grpc_call` check a gRPC target as the `http` or `https` origin it is reached over (`grpc://h:1` as `http://h:1`); `grpc://` and `grpcs://` values of the active environment count for the allowlist like `http(s)` ones. `grpc_call` calls unary and server-streaming methods only. Every gRPC call is a POST, so the method rule reads the proto instead: a method marked `option idempotency_level = NO_SIDE_EFFECTS;` is a read, and any other method is a mutation, which needs `allow_mutations` and the human's confirmation like an HTTP POST. See [gRPC](grpc.md#agent-tools).

`realtime_listen` opens a WebSocket or SSE session and returns the messages it received, after at most 100 messages or 60 s. Its URL must pass the allowlist (a `ws://` URL is checked as its `http://` origin, `wss://` as `https://`), and sending frames is a mutation: refused with `blocked: sending frames requires allow_mutations` unless `[agent] allow_mutations`, and confirmed by the human otherwise. See [WebSocket and SSE](realtime.md#agent-tool).

`send_request` and `run_collection` compare [snapshots](snapshots.md) in CI mode: an agent never writes or updates a snapshot file, and a missing snapshot fails the assertion.

### Human confirmation

When mutations are allowed, each mutating request first asks the human, through MCP elicitation, with the method, the redacted URL and the environment:

```text
An agent wants to send DELETE http://127.0.0.1:8080/users/7 (environment 'local'). Allow it?
```

Only an explicit yes sends the request. Declining or dismissing the question gets `code: "declined"`. A client that doesn't support elicitation gets `code: "confirmation_unavailable"`, unless `require_confirmation = false`: that setting accepts unattended mutations and should be used only when you trust the agent's every call. A client that supports elicitation is always asked, whatever the setting. If asking fails, the call fails with `confirmation_unavailable`.

Clients that speak the stateless `2026-07-28` revision can't receive elicitation requests from the server, so for them mutations fail closed with `confirmation_unavailable` (or need `require_confirmation = false`).

## Tools

Every tool declares an input and an output JSON Schema, and returns `structuredContent` that conforms to its output schema, plus a one-line summary as text. Tools are thin adapters over the same engine calls the CLI makes.

The reference below is generated from the tool schemas that `tools/list` returns; a test keeps it in sync.

<!-- tool-reference:start: generated from the tool schemas, do not edit by hand -->

| Tool | Arguments (**required**) | Read-only | Description |
|------|------|------|------|
| `list_requests` | `collection`, `kind`, `text`, `cursor`, `limit` | yes | List the workspace's saved requests, filtered by collection folder, kind or text, 50 per page. |
| `get_request` | **`path`** | yes | Read one saved request file by its id or path. Secrets appear as {{name}} references, never as values. |
| `list_environments` | none | yes | List the environments: names, which is active in this session, and variable and secret names. |
| `select_environment` | **`name`** | yes | Make an environment active for this session only; nothing is written to disk. |
| `send_request` | `path`, `vars`, `method`, `url`, `headers`, `body`, `timeout_ms` | no | Send a saved request (path, optional vars) or an ad-hoc one (method, url, headers, body) through the safety policy. Returns the redacted exchange. |
| `run_collection` | `path`, `filter`, `vars`, `bail` | no | Run the requests under a folder or file like `rustman run`, in the background. Returns a job_id; get_report with it gives the run report. |
| `import_openapi` | **`spec`**, `into`, `dry_run` | no | Import an OpenAPI document (a workspace file or an allowed URL) as a request collection. dry_run (the default) only reports what would change. |
| `explore_plan` | **`spec`**, **`target`**, `probes`, `operations`, `seed`, `fuzz_cases`, `allow_mutations`, `allow_destructive`, `cursor`, `limit` | yes | Plan the probes of an exploration against a target without sending anything. Review it before explore_run. |
| `explore_run` | **`spec`**, **`target`**, `probes`, `operations`, `seed`, `fuzz_cases`, `allow_mutations`, `allow_destructive`, `cursor`, `limit`, `max_probes`, `max_duration_ms`, `fail_on` | no | Start an exploration in the background (read-only unless rustman.toml allows more). Returns a job_id and exploration_id. |
| `explore_status` | `id` | yes | The phase, progress and findings by severity of an exploration, by job_id or exploration_id (default: the latest). |
| `explore_cancel` | **`job_id`** | no | Stop a running exploration job. It stays resumable with `rustman explore resume <id>`. |
| `get_report` | `id`, `format` | yes | A report by exploration id, run job id or JSON report file of the workspace (such as out/report.json): a summary (default), or rendered as json, markdown, junit, sarif or html. |
| `list_findings` | `exploration_id`, `severity`, `operation`, `cursor`, `limit` | yes | The findings of an exploration (default: the latest), filtered by minimum severity and operation, 50 per page. |
| `search_history` | `url`, `method`, `status`, `since`, `until`, `cursor`, `limit` | yes | Search recorded exchanges by URL text, method, status range (500-599, 5xx, 404) and time range, newest first, 50 per page. |
| `rpc_call` | `path`, `endpoint`, `method`, `params`, `vars`, `timeout_ms` | no | Send one JSON-RPC call (endpoint, method, params) or a saved jsonrpc request through the safety policy. Only methods matching [jsonrpc] agent_read_only_methods are sent; subscriptions are refused. |
| `realtime_listen` | `path`, `url`, `frames`, `max_messages`, `duration_ms`, `vars` | no | Open a WebSocket (ws:// or wss:// url) or SSE (http(s):// url) session, or a saved ws or sse request, through the safety policy, optionally send initial frames (a mutation: needs [agent] allow_mutations), and return the redacted messages received, after at most 100 messages or 60 s. |
| `grpc_list` | `target`, `path`, `refresh`, `vars` | yes | List the services and methods (with call types) of a gRPC target or saved grpc request, by reflection or its schema, through the safety policy's allowlist. |
| `grpc_describe` | **`symbol`**, `target`, `path`, `refresh`, `vars` | yes | Describe a gRPC service, method (with a request message template) or message of a target or saved grpc request. |
| `grpc_call` | `path`, `target`, `method`, `message`, `metadata`, `deadline_ms`, `vars` | no | Call a unary or server-streaming gRPC method (target, method, message, metadata) or a saved grpc request through the safety policy. Returns the redacted status, metadata and messages; streams stop at 100 messages or 30 s. |

<!-- tool-reference:end -->

`graphql_introspect`, `graphql_query` and `list_webhook_captures` are registered only in builds with the GraphQL and webhook inspector capabilities, which this version doesn't include. `grpc_list`, `grpc_describe` and `grpc_call` are registered when `rustman-mcp` is built with its default `grpc` feature.

Notes:

- Saved and ad-hoc requests go through the runner like `rustman send`: variables, auth, assertions, captures and history all apply. Requests with OAuth2 auth can't be sent by agents, because the token flow may need a browser; run `rustman auth login` and send them yourself. Requests with Digest auth are refused with an error rather than sent without credentials, because the agent path does not answer the challenge; send them with `rustman send`.
- `explore_plan` and `explore_run` read the document from a workspace file or from an `http(s)` URL on a host agents may reach. The target must be such a host too. The seed defaults to `0`, so a plan stays the same between its pages and the `explore_run` that follows; pass `seed` to vary the fuzz cases. Probes are read-only unless the agent asks for `allow_mutations` (or `allow_destructive`) and `[agent]` allows it; a mutating exploration asks the human first.
- `import_openapi` with `dry_run: false` writes workspace files, so it asks the human first, like a mutation (and fails with `confirmation_unavailable` when the client can't ask, unless `require_confirmation = false`). The running session keeps the request list it started with; reconnect to see the new files.
- `run_collection` and `explore_run` start background jobs; see below.

## Background jobs

`explore_run` and `run_collection` answer within 2 seconds with a `job_id` and keep working in the background, so an agent never waits on a long call. At most `max_jobs` jobs (2 by default) run at a time across every session of the server; a further job-starting call, from any session, gets `code: "busy"` until one ends. A cancelled job keeps its slot until its work has really stopped.

- **Status.** `explore_status` reports an exploration's state (`running`, `completed`, `cancelled`, `interrupted` or `failed`), phase, probes done out of the total, and findings by severity. `get_report` with a run's `job_id` returns the run report once the run ends; while it runs, the error message says how far it got (`job-1 is still running (2/5 requests)`). A report file is read only inside the workspace (a symlink leading out of it is refused) and up to 32 MiB; a file that is not a report is refused without quoting it.
- **Progress.** When the `tools/call` that started a job carried a progress token (`_meta.progressToken`), the server sends `notifications/progress` for that token every 2 seconds while the job runs, and once more when it ends. `progress` counts probes or requests done, `total` is the job's size, and `message` reads like `job-1 running: 12/51 probes`. Messages are redacted like every other output. The `tools/call` itself has already answered with the `job_id`, so these notifications arrive after its response; this is how the server keeps a long job visible without making the agent wait. The MCP specification expects progress to stop when its request completes, so a strict client may drop them or log them under an unknown token. Poll `explore_status` or `get_report` when yours does.
- **Cancellation.** `explore_cancel` with the `job_id` (or exploration id) stops an exploration; an MCP `notifications/cancelled` for the `tools/call` that started an exploration or a run does the same, if the client still sends one after that call answered (many clients only cancel requests still pending, so prefer `explore_cancel`). Cancelling stops dispatching new probes or requests, lets those in flight finish for up to 10 seconds, and still cleans up what the probes created, exactly like Ctrl+C on `rustman explore run`. The job's status becomes `cancelled`.
- **Resuming.** An exploration is journaled under `.rustman/explorations/<id>/` like one started from the terminal, so a cancelled exploration stays resumable: `rustman explore resume <exploration_id>` runs the probes that did not finish, and never sends one twice. A run's report stays available for the session; an exploration's report is written next to its journal.

## Resources

| URI | Content |
|-----|---------|
| `rustman://requests/<path>` | A request file of the workspace, by its path (`users/get-user.rman.yaml`) |
| `rustman://reports/<id>` | A `rustman.report/v1` report: an exploration id, or the job id of a finished run |
| `rustman://schemas/<name>` | `report` (the report JSON Schema) or an imported OpenAPI snapshot from `.rustman/openapi/` |
| `rustman://bodies/<id>` | The whole redacted body of a truncated response (`body_ref`) |

Every content is redacted. A path with `..`, a root, a drive or a backslash is refused before anything is read, and only files the workspace loaded as requests are readable under `rustman://requests/`.

## Prompts

- `explore-api` (arguments `spec`, `target`) guides an agent through `explore_plan`, reviewing the plan with the human, `explore_run`, polling `explore_status`, then `get_report` and `list_findings`.
- `triage-findings` (argument `exploration_id`) guides an agent through grouping an exploration's findings by root cause, with reproduction steps and a proposed fix per cause.

## Secrets and redaction

An agent can refer to secrets by name but never read their values, in any form. Engine output is already redacted; the server then runs a second pass over the final JSON of every tool result, resource, log and progress message, which replaces:

- every value of the session environment's secrets, and of the secrets and credentials of every request sent in the session, in the forms the [redactor](environments.md) matches (JSON, URL and HTML encodings) plus their base64 forms (standard and URL-safe, padded or not), with `«redacted:<name>»`;
- the values of the `Authorization`, `Proxy-Authorization`, `Cookie`, `Set-Cookie` and `X-Api-Key` headers wherever a header appears, with `«redacted:<Header-Name>»`;
- literal credentials pasted into tracked request and collection files: those headers' values and the credentials of `auth` blocks (a Basic password, a Bearer token, an API key value). They are redacted in every output, including the file text `get_request` and `rustman://requests/` return. Literals shorter than 6 characters are too common to replace everywhere and stay visible in file text; use a `{{secret}}` reference for any credential.

A backend that echoes `Authorization: Bearer <api_token>` into its body is shown as `Bearer «redacted:api_token»`. Other transforms of a secret (hashing, encryption, custom encodings) are not recognized.

## Output limits

- A tool result whose JSON exceeds `max_output_bytes` (256 KiB by default) is refused with `code: "invalid_input"`: narrow the request or page through it.
- A response body over 32 KiB is cut to its first 32 KiB with `truncated: true`, its full `size` in bytes, and a `body_ref` such as `rustman://bodies/b3`. The whole redacted body stays readable through `resources/read` while the session keeps it: each session keeps up to 64 MiB of bodies, dropping the least recently used first.
- Lists return 50 items by default with a `next_cursor` when more exist. Pass it back to get the next page; the last page has none. A cursor from a list that has changed since is refused.

## Audit log

Every tool call appends one JSON line to `.rustman/audit/<YYYY-MM-DD>.ndjson` (UTC date) with the timestamp, session id, client name, tool, redacted arguments, outcome (`ok`, `error`, `blocked` or `declined`), the reason of a failed call and, for outbound requests, the method and redacted URL:

```json
{"timestamp":"2026-11-18T10:04:12.311Z","session":"9f1c…","client":"claude-code","tool":"send_request","arguments":{"method":"POST","url":"http://127.0.0.1:8080/users"},"outcome":"blocked","reason":"mutations not allowed for agents","method":"POST","url":"http://127.0.0.1:8080/users"}
```

Lines are appended with one write each; the server never rewrites or deletes them. Retention is up to you (the files are small). The audit log fails closed: if it can't be written, the call is refused before it runs, or fails if the line can't be appended afterwards. A background job checks it before each request it sends, so once the log can't be written the job sends nothing more. `.rustman/` is git-ignored by `rustman init`.

## Error codes

Tool errors are tool results with `isError: true` and `structuredContent` `{"code": "...", "message": "..."}`, not protocol errors:

| Code | Meaning |
|------|---------|
| `not_found` | The request, environment, report or resource does not exist |
| `invalid_input` | Malformed arguments, a stale cursor, or a result over `max_output_bytes` |
| `blocked` | Refused by the agent permissions or the safety policy; the message is the reason |
| `declined` | The human declined or dismissed the confirmation |
| `confirmation_unavailable` | A mutating call needs confirmation and the client can't ask |
| `busy` | Too many background jobs are running (`max_jobs`) |
| `internal` | Anything else, such as a network error or an audit log that can't be written |
