# Request history

Rustman keeps a local, redacted record of the exchanges it sends, so you can look back at what happened, search it and send a request again. `rustman send`, `rustman run` and `rustman history replay` all record; later front-ends (the TUI and the agent interface) read the same store.

## What is recorded

Every request that was resolved and sent is recorded as one exchange, whether it got a response or failed with a network, TLS or timeout error. A request that never left (an unresolved variable, a `--bail` skip, Ctrl+C before it was sent) is not recorded. An exchange holds:

- `id`: a [ULID](https://github.com/ulid/spec), so ids sort by time;
- `timestamp` (RFC 3339, UTC), `request` (the request file relative to the workspace root, or `adhoc` for `rustman send <METHOD> <URL>`) and `environment`;
- `sent`: the method, URL with its query, headers and text body as sent;
- `status`, `total_ms` and `error`;
- `record`: the response (status, headers, body), timings, redirects, assertion results and captures, as in a JSON report; for a GraphQL request also `graphql`, with the operation, its redacted variables, the result status and a subscription's events (see [GraphQL](graphql.md#history));
- `spill`: for a body too large to keep in memory (see `[http] max_body_bytes` in [HTTP requests](http.md)), the file under `.rustman/bodies/` that holds it; `record` then keeps only its start;
- `replay`: the request as `rustman history replay` sends it again (see [Replay](#replay)).

Each exchange is one JSON file, `.rustman/history/<id>.json`, with `"schema": "rustman.history/v1"`.

## Where it is stored

- Inside a workspace: `.rustman/history/`, next to the cookie jars and persisted captures. `.rustman/` must be git-ignored (`rustman init` adds it to `.gitignore`); Rustman refuses to write there otherwise, as it does for secrets.
- Ad-hoc requests sent outside any workspace: `<data_dir>/rustman/history/`, where `<data_dir>` is `$XDG_DATA_HOME` (or `~/.local/share`) on Linux, `~/Library/Application Support` on macOS and `%APPDATA%` on Windows. Set `RUSTMAN_DATA_DIR` to use another folder (`$RUSTMAN_DATA_DIR/history/`).

`rustman history` uses the workspace found from the current folder (or `--workspace DIR`), and the ad-hoc history outside any workspace.

## Turning it off

```toml
# rustman.toml
[workspace]
name = "shop"
version = 1

[history]
# Record exchanges (default: true).
enabled = true
# How many exchanges to keep (default: 500).
max_entries = 500
```

`--no-history` on `rustman send`, `rustman run` and `rustman history replay` skips recording for that command only.

## Retention

After each write, the oldest exchanges beyond `max_entries` are deleted, then their spilled bodies, along with any spilled body no entry refers to that is more than an hour old. On Windows, a file another program holds open right now is kept silently and deleted by a later write. `rustman history clear` deletes every exchange and every spilled body in `.rustman/bodies/`.

Entries are readable by the current user only, like the secrets file: mode `0600` on Unix, and on Windows the current user's grant without inherited permissions. Writes are safe across parallel runs and processes: each exchange is written to a temporary file, flushed to disk and renamed into place, so readers never see half an entry, and pruning holds an exclusive lock on `.rustman/history/.lock`. An entry that cannot be read (a file damaged by hand, say) is skipped with a `warning:` instead of failing the listing.

## Redacted at rest

History is written through the same redaction as every other output (see [Environments, variables and secrets](environments.md)): secret values become `«redacted:<name>»`, and the values of the `Authorization`, `Proxy-Authorization`, `Cookie` and `Set-Cookie` headers become `«redacted:<Header-Name>»`, including when a server echoes them back. `--show-secrets` never changes what is stored.

## Listing and searching

`rustman history list` prints one line per exchange, newest first: id, time, method, URL, status and total time.

| Flag | Keeps |
|------|-------|
| `--status <code\|class>` | that status (`404`) or class (`5xx`) |
| `--method <m>` | that method, ignoring case |
| `--grep <text>` | exchanges whose URL, request body or response body contains the text, ignoring case (only the stored start of a spilled body is searched) |
| `--since <duration>` | exchanges from the last `30s`, `15m`, `1h`, `2d` or `1w` |
| `--limit <n>` | the newest `n` matches (`n` is at least 1) |
| `--json` | (output) a JSON array of `{id, timestamp, request, environment, method, url, status, total_ms, error}` |

`rustman history show <id>` prints one exchange in full (`--json` prints the stored entry). Ids can be shortened to any unique, non-empty prefix. An entry that cannot be read fails with `error: cannot read history entry <path>: <reason>` (exit code 2).

## Replay

`rustman history replay <id>` sends the stored request again and records it as a new exchange. It takes `--body-only`, `-o FILE`, `--show-secrets` and `--no-history` like `rustman send`, and prints the response the same way. The stored request has `{{name}}` placeholders where secrets were, and replay reads them from the secret store at that moment: a rotated secret is sent with its new value, and a deleted one fails with `unresolved variable(s): <name>` before anything is sent. There are no assertions on a replay; a response is a pass (exit code 0).

Some exchanges cannot be replayed, because what they sent is deliberately not kept:

- a sensitive header (`Authorization`, `Cookie`...) or an auth credential written literally instead of through a secret variable (only a scheme such as `Bearer ` may be written around the secret);
- a multipart or file body;
- a GraphQL subscription (its WebSocket or event stream is not a plain exchange);
- a value derived from a secret: a captured value that contained a secret (see [Captures](assertions.md#captures)). History stores `{{<var>}}` in its place, or, if any other stored part still held a secret, nothing but the method.

Replaying one of them fails with `this exchange cannot be replayed: ...`; send the request from its file instead.

## Example

A workspace with a secret token, and a request that uses it:

```yaml
# environments/dev.yaml
variables:
  base_url: http://localhost:8080
secrets:
  - api_token
```

```yaml
# users/me.rman.yaml
version: 1
kind: http
name: me
url: '{{base_url}}/me'
headers:
  - name: Authorization
    value: 'Bearer {{api_token}}'
```

Send it, find it in history, and replay it:

```console
$ rustman send me --env dev --body-only
{"id":42}
$ rustman history list
01JD8X3F6W2N7Q4R5S6T7V8W9X  2026-11-17T09:30:00.123Z  GET http://localhost:8080/me  200  3 ms
$ rustman history replay 01JD8X3F6W2N7Q4R5S6T7V8W9X --body-only
{"id":42}
$ rustman history list --status 2xx --since 1h --limit 1
01JD8X4A1B2C3D4E5F6G7H8J9K  2026-11-17T09:31:12.456Z  GET http://localhost:8080/me  200  2 ms
```

The stored entries show `Authorization: «redacted:Authorization»`, and their `replay` part holds `Bearer {{api_token}}`. Once the secret is deleted, replay stops before sending:

```console
$ rustman secret rm api_token --env dev
removed secret 'api_token' from environment 'dev'
$ rustman history replay 01JD8X3F6W2N7Q4R5S6T7V8W9X
error: unresolved variable(s): api_token (secret not set; run 'rustman secret set api_token --env dev')
$ rustman history clear
deleted 2 exchanges from history
```

## Exit codes

`history list`, `show` and `clear` exit 0, or 2 on a bad flag, an unknown id or an unreadable store. `history replay` exits like `rustman send`: 0 with a response, 3 when the request could not be completed (including an unresolved secret or an exchange that cannot be replayed), 130 on Ctrl+C. See the [CLI reference](cli.md).
