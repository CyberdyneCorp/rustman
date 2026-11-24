# Snapshot testing

A **snapshot** records a known-good response once and fails later runs when the response changes, with a readable diff. Normalization keeps timestamps, ids and other noise out of the comparison, and secrets are redacted before anything is stored. Snapshot files are plain YAML next to the request, so a response change shows up in code review.

The examples on this page are checked by an integration test (`crates/rustman-core/tests/docs_snapshots.rs`): every request file loads, every snapshot file is read back and written again byte for byte, and each response shown is normalized into exactly the snapshot shown after it.

## The `snapshot` assertion

Add `snapshot` to a request file's `assertions` list. The shorthand compares the status and the body with the default rules:

```yaml
# users/get-user.rman.yaml
version: 1
kind: http
name: Get user
url: '{{base_url}}/users/42'
assertions:
  - status: 200
  - snapshot
```

The full form takes any of `name`, `compare`, `ignore`, `headers`, `replace`, `sort` and `presets`, all optional:

```yaml
# users/list-users.rman.yaml
version: 1
kind: http
name: List users
url: '{{base_url}}/users'
assertions:
  - snapshot:
      name: listing
      compare: [status, headers, body]
      ignore: ['$.request_id']
      headers:
        include: [content-type, x-api-version]
      replace:
        - pattern: 'ORD-\d+'
          with: 'ORD-«n»'
      sort:
        - path: $.users
          by: id
      presets: [timestamps, uuids, etags]
```

- A request file may hold several snapshot assertions; each needs its own `name` (the default name is `default`). Two with the same name fail to load with `<path>:<line>: duplicate snapshot name 'default'`.
- Names may use letters, digits, `-` and `_`, since they become part of a file name.
- Snapshots work for every request kind that produces a response: `http`, `graphql`, `jsonrpc` and `mcp`. For `jsonrpc`, each call's snapshot holds that call's response envelope (`result` or `error`); in a batch, names are unique across all the calls (see [JSON-RPC snapshots](jsonrpc.md#snapshots)).
- With `--iterations` above 1, every iteration compares against the same snapshot.

## Where snapshots are stored

Each snapshot is a file in a `__snapshots__/` folder next to the request file, named `<request-file-stem>.<name>.snap.yaml`. `users/get-user.rman.yaml` with a `default` snapshot is stored in `users/__snapshots__/get-user.default.snap.yaml`.

Snapshot files belong in Git: `rustman init` never adds `__snapshots__/` to `.gitignore`.

## The snapshot file

```yaml
# users/__snapshots__/get-user.default.snap.yaml
version: 1
request: users/get-user.rman.yaml
name: default
kind: http
compare:
  - status
  - body
status: 200
body:
  json:
    id: 42
    name: Ada
    roles:
      - admin
    updated: «timestamp»
```

- `version` is `1`. `request` is the request file's path from the workspace root, always with `/`. `name`, `kind` and `compare` say which assertion wrote it.
- `status`, `headers` and `body` are present when compared.
- The body is one of:
  - `json`: the JSON body as a YAML structure, every object's keys sorted;
  - `text`: any other text, as a literal block;
  - `binary`: `{sha256, size}` for bodies that are not valid UTF-8 or whose content type is not textual (images, archives...), and for bodies larger than 1 MiB after normalization (with a warning suggesting `ignore` rules);
  - `events`: the event list of a subscription (see [Subscriptions](#subscriptions-and-event-lists)).
- Files use 2-space indentation and end with a newline. Two runs producing the same normalized response produce byte-identical files, whatever the order of the keys in the response.

## What is compared

`compare` lists the parts of the response that are stored and compared: `status`, `headers` and `body`. The default is `[status, body]`.

When headers are compared, only the headers listed in `headers.include` are stored (default: `content-type`), with lower-case names, sorted. Repeated headers are joined with `, `. A header that changes on every response, such as `Date`, therefore never fails a snapshot unless it is included. Included headers listed in `headers.redact` are stored as `«redacted»`, which checks that the header is present without storing its value.

```yaml
# catalog/get-item.rman.yaml
version: 1
kind: http
name: Get item
url: '{{base_url}}/items/7'
assertions:
  - snapshot:
      compare: [status, headers, body]
      headers:
        include: [content-type, x-request-id]
        redact: [x-request-id]
```

```json
// response: 200
// Content-Type: application/json
// Date: Tue, 17 Nov 2026 10:00:00 GMT
// X-Request-Id: 7d2f9c
{"id": 7, "title": "Lamp"}
```

```yaml
# catalog/__snapshots__/get-item.default.snap.yaml
version: 1
request: catalog/get-item.rman.yaml
name: default
kind: http
compare:
  - status
  - headers
  - body
status: 200
headers:
  content-type: application/json
  x-request-id: «redacted»
body:
  json:
    id: 7
    title: Lamp
```

If `content-type` later changes from `application/json` to `text/html`, the snapshot fails with:

```text
changed headers.content-type: "application/json" → "text/html"
```

## Normalization

Before a response is stored or compared, it is normalized in a fixed order:

1. secret redaction;
2. `ignore`;
3. `presets`;
4. `replace`;
5. `sort`;
6. the canonical form (object keys sorted).

The same normalized form is used for storing and for comparing, and normalizing a response twice gives identical output.

### Secret redaction

Secret values and the sensitive headers (`Authorization`, `Proxy-Authorization`, `Cookie`, `Set-Cookie`) are replaced with `«redacted:<name>»` first, with the same redaction every other output uses (see [Environments](environments.md)). This includes secrets a server echoes back inside a body, in the encoded forms listed under [Redaction](environments.md#redaction). A snapshot file never contains a resolved secret value, even with `--update-snapshots`; if a `replace` rule would put one back, the snapshot is not written and the assertion reports an error.

Snapshot files are meant to be committed, so know the limits of that redaction before you commit one:

- A secret the server transforms before echoing it (base64-encodes it, hashes it, changes its case, splits it) is not recognized and is stored as the server sent it. Add an `ignore` or `replace` rule for such a field.
- Only the secrets the requests of the command reference are known (see [Redaction](environments.md#redaction)). An environment secret no request of the command uses is not read from the store, so a server that returns it anyway gets it stored in plain text, and the check before writing cannot catch it either.

Review a new snapshot file before committing it, as you would any other file.

```yaml
# auth/whoami.rman.yaml
version: 1
kind: http
name: Who am I
url: '{{base_url}}/whoami'
headers:
  - name: X-Token
    value: '{{token}}'
assertions:
  - snapshot
```

With the secret `token` set to `abc123`, and a server echoing it:

```json
// response: 200
// Content-Type: application/json
{"user": "ada", "token": "abc123"}
```

```yaml
# auth/__snapshots__/whoami.default.snap.yaml
version: 1
request: auth/whoami.rman.yaml
name: default
kind: http
compare:
  - status
  - body
status: 200
body:
  json:
    token: «redacted:token»
    user: ada
```

### Ignored paths

`ignore` is a list of [JSONPath](assertions.md) expressions. Every value they match is stored as `«ignored»`, so it can change freely while the field itself must still be there. A path that matches nothing is not an error. For an event list, the paths are applied to each event.

```yaml
# orders/create-order.rman.yaml
version: 1
kind: http
name: Create order
method: POST
url: '{{base_url}}/orders'
assertions:
  - snapshot:
      ignore: ['$.id', '$.items[*].created_at']
```

```json
// response: 201
// Content-Type: application/json
{"id": 981, "items": [{"sku": "A1", "created_at": "yesterday"}]}
```

```yaml
# orders/__snapshots__/create-order.default.snap.yaml
version: 1
request: orders/create-order.rman.yaml
name: default
kind: http
compare:
  - status
  - body
status: 201
body:
  json:
    id: «ignored»
    items:
      - created_at: «ignored»
        sku: A1
```

### Presets

`presets` replace common kinds of changing values with a typed placeholder:

| Preset | Matches | Placeholder |
|---|---|---|
| `timestamps` | RFC 3339 and ISO 8601 date-times such as `2026-11-17T10:00:00Z` or `2026-11-17 10:00` | `«timestamp»` |
| `dates` | calendar dates such as `2026-11-17`, and HTTP dates such as `Tue, 17 Nov 2026 10:00:00 GMT` | `«date»` |
| `uuids` | UUIDs in any case | `«uuid»` |
| `etags` | a whole value that is an entity tag, `"33a64df5"` or `W/"33a64df5"` | `«etag»` |
| `epoch_ms` | 13-digit integers (or strings of 13 digits) in fields named `*_at`, `*time*` or `timestamp` | `«epoch_ms»` |
| `jwt` | JSON Web Tokens (`eyJ...`) | `«jwt»` |

The default is `[timestamps, uuids]`. `presets: []` turns every preset off, and a list replaces the defaults (list `timestamps` again to keep it). Presets apply to every string of a JSON body, to the whole of a text body, and to stored header values. They run in the order of the table whatever order they are listed in, so a timestamp is never half-replaced as a date.

```yaml
# sessions/start.rman.yaml
version: 1
kind: http
name: Start session
method: POST
url: '{{base_url}}/sessions'
assertions:
  - snapshot:
      presets: [timestamps, dates, uuids, epoch_ms, jwt]
```

```json
// response: 200
// Content-Type: application/json
{"id": "3f2504e0-4f89-11d3-9a0c-0305e82c3301", "started": "2026-11-17T10:00:00Z", "day": "2026-11-17", "expires_at": 1763373600000, "token": "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2ln"}
```

```yaml
# sessions/__snapshots__/start.default.snap.yaml
version: 1
request: sessions/start.rman.yaml
name: default
kind: http
compare:
  - status
  - body
status: 200
body:
  json:
    day: «date»
    expires_at: «epoch_ms»
    id: «uuid»
    started: «timestamp»
    token: «jwt»
```

With `presets: []`, `"updated":"2026-11-17T10:00:00Z"` is stored verbatim.

### Regex replacements

`replace` is a list of `{pattern, with}` rules, applied in order after `ignore` and the presets: to every string value of a JSON body, or to the whole text of any other body. `with` may use `$1` or `${name}` for capture groups. An invalid regular expression fails when the file loads, with `<path>:<line>: invalid regex '(': <reason>`.

```yaml
# orders/get-order.rman.yaml
version: 1
kind: http
name: Get order
url: '{{base_url}}/orders/latest'
assertions:
  - snapshot:
      replace:
        - pattern: 'ORD-\d+'
          with: 'ORD-«n»'
```

```text
// response: 200
// Content-Type: text/plain
Order ORD-123 shipped.
```

```yaml
# orders/__snapshots__/get-order.default.snap.yaml
version: 1
request: orders/get-order.rman.yaml
name: default
kind: http
compare:
  - status
  - body
status: 200
body:
  text: Order ORD-«n» shipped.
```

`ORD-987` normalizes to the same value, so the snapshot keeps matching.

### Array ordering

Array order is significant by default. `sort` is a list of `{path, by}` rules: the array at the JSONPath `path` is sorted by the value at `by`, a key path relative to each item (`id`, `owner.id`), or by the whole item when `by` is omitted. Numbers sort numerically and strings by their characters.

```yaml
# users/list-team.rman.yaml
version: 1
kind: http
name: List team
url: '{{base_url}}/team'
assertions:
  - snapshot:
      sort:
        - path: $.users
          by: id
        - path: $.tags
```

```json
// response: 200
// Content-Type: application/json
{"users": [{"id": 10, "name": "Lin"}, {"id": 2, "name": "Ada"}], "tags": ["ops", "dev"]}
```

```yaml
# users/__snapshots__/list-team.default.snap.yaml
version: 1
request: users/list-team.rman.yaml
name: default
kind: http
compare:
  - status
  - body
status: 200
body:
  json:
    tags:
      - dev
      - ops
    users:
      - id: 2
        name: Ada
      - id: 10
        name: Lin
```

## Diffs

When a response no longer matches, the failure shows what changed:

- **JSON bodies**: one line per change, with its JSONPath, its kind (`added`, `removed` or `changed`) and the old and new values. At most 50 changes are shown, followed by `… and 30 more changes` (for 80). Arrays compare by index, after any `sort` rule.

  ```text
  changed $.user.role: "admin" → "viewer"
  added $.user.email: "ada@example.com"
  removed $.legacy: true
  ```

- **Status and headers**: the same lines, with `status` and `headers.<name>` as the path.
- **Text bodies**: a unified line diff with 3 lines of context, from the stored snapshot (`---`) to the received response (`+++`).
- **Binary bodies**: the old and new size and hash:

  ```text
  binary body changed: 10240 B sha256:ab12… → 10301 B sha256:9f0c…
  ```

- **A body that changed form** (JSON to text, for example): `body type changed: json → text`.

## Subscriptions and event lists

For exchanges with a bounded event list (GraphQL subscriptions, JSON-RPC subscriptions), the snapshot body is the list of events, each normalized on its own. When a run collects a different number of events than the snapshot holds, the failure starts with `event count changed: 3 → 2`, followed by the diff of the events.

```yaml
# live/__snapshots__/on-price.default.snap.yaml
version: 1
request: live/on-price.rman.yaml
name: default
kind: graphql
compare:
  - status
  - body
body:
  events:
    - data:
        price: 10
    - data:
        price: 11
```

## Recording, updating and CI

- **First run**: when a snapshot file does not exist, it is written and the assertion is reported `recorded` (it counts as passed).
- **Later runs** compare against the stored file: `matched` or `failed`.
- **Update mode** (`--update-snapshots`) overwrites differing snapshots and reports them `updated`. Matching snapshots are never rewritten, so their bytes and modification times stay the same.
- **CI mode** (`--ci`, or the environment variable `CI` set to `true` or `1`) never writes snapshot files. A missing snapshot fails with `snapshot missing: <path> (run without --ci to record)`.
- `--ci` (or `CI=true`) together with `--update-snapshots` is a usage error: `rustman run` exits with code 2 and `error: --update-snapshots cannot be used in CI mode`, before anything is sent.
- `rustman send` and `rustman graphql query` take the same `--update-snapshots` (`-u`) and `--ci` flags and honor `CI` the same way; they note each recorded or updated snapshot on stderr, as in `snapshot default recorded: users/__snapshots__/get-user.default.snap.yaml`.
- Agents ([`rustman mcp serve`](agent.md)) always compare in CI mode and never write snapshot files.

```sh
rustman run users/                      # records new snapshots, compares the others
rustman run users/ --update-snapshots   # accepts intended changes (-u for short)
rustman run . --ci                      # in a pipeline: never writes, a missing snapshot fails
```

A failed or missing snapshot fails its request like any other assertion (exit code 1). Recorded and updated snapshots count as passed, so an update run exits 0 when nothing else failed.

Snapshot files are written through a temporary file that is renamed into place, under a per-file lock, so a run and an approval in the TUI never leave half a file. A write only replaces the file it compared against: when another run or an approval changed the snapshot in the meantime, the write is skipped with `<path>: the snapshot file changed on disk since it was compared; it was not written (run again)`. A temporary file (`.<name>.snap.yaml.<pid>-<n>.tmp`) left by an interrupted write is removed by the next write of that snapshot, and is safe to delete. Snapshot files checked out with Windows line endings still match.

## Reviewing in the TUI

Sending a request with a snapshot from the [TUI](tui.md#response-viewer) records, compares and stores snapshots the same way as `rustman run` (in CI mode when `CI` is `true` or `1`). When a snapshot failed, is missing or was just recorded, the response viewer gets a **Snapshot** tab after `Assertions` with each snapshot's state, its file and the diff (a recorded or missing snapshot shows the received snapshot instead):

```text
snapshot default failed · users/__snapshots__/get-user.default.snap.yaml
changed $.name: "Bob" → "Ana"
added $.role: "admin"

a approve · r reject · Alt+A review all
```

- `a` approves the next open snapshot: the received snapshot is written over the stored file (atomically, under its lock) and the tab shows `approved`.
- `r` rejects it: the file is left unchanged, the assertion stays failed and the tab shows `rejected`.
- After sending several requests, `Alt+A` opens a review queue that steps through every failing snapshot still open in any tab: `a` approves and `r` rejects the current one and moves to the next, `n`/`p` (or the arrow keys) step without deciding, and `Esc` closes it. The queue closes by itself after the last decision.
- In CI mode approving is disabled: the tab says `Approval is disabled in CI mode.` and `a` only shows a warning.

`Ctrl+Shift+<letter>` is not used because most terminals cannot tell it from `Ctrl+<letter>`. Everything the tab and the queue show is redacted, like the snapshot files themselves.

## In reports

Every reporter shows the snapshot outcomes: `matched`, `recorded`, `updated`, `failed` or `missing`. The [pretty report](cli.md#reporters) lists the files a run recorded or updated under their request, the diff under a failed one, and a summary line with the counts:

```text
PASS  users/get-user  GET 200  12 ms
      snapshot default recorded: users/__snapshots__/get-user.default.snap.yaml
FAIL  users/list-users  GET 200  9 ms
      snapshot mismatch: users/__snapshots__/list-users.listing.snap.yaml
      changed $.users[0].role: "admin" → "viewer"

2 requests: 1 passed, 1 failed, 0 errored, 0 skipped (21 ms)
1 snapshot recorded, 1 snapshot failed
```

- **JSON** (`rustman.report/v1`, additive fields): each snapshot assertion (`"kind": "snapshot"`) has a `snapshot` object with `name`, `state`, `path` (the snapshot file, relative to the workspace, with `/`) and, when it failed or was updated, the structured `diff` (`changes` with `path`, `kind`, `old` and `new`; `more`; and `text`, `binary`, `event_count` or `body_type` when they apply). `summary.snapshots` counts the states; both are absent from runs without snapshots. See the [report schema](schemas/report.schema.json).
- **JUnit**: a failed snapshot's `<failure>` message holds the diff text; recorded and updated snapshots are listed in the test case's `<system-out>`, and the counts are the suite property `snapshots`.
- **TAP**: the diff is a reason in the failed test point's YAML block; recorded and updated snapshots and the counts are `#` comments.

Every diff is redacted like the rest of the report.

## Listing and pruning

`rustman snapshot list [path]` lists every snapshot file under a folder or request file (default: the whole workspace) with its state, without sending anything:

```text
ok                 users/__snapshots__/get-user.default.snap.yaml  users/get-user.rman.yaml default
unreferenced name  users/__snapshots__/get-user.v1.snap.yaml  users/get-user.rman.yaml v1
orphaned           users/__snapshots__/old.default.snap.yaml  users/old.rman.yaml default
```

- `ok`: the request file declares a snapshot with that name (or is a kind this version cannot read, which is never pruned).
- `orphaned`: the request file no longer exists.
- `unreferenced name`: the request file no longer declares that snapshot name.

`--json` prints an array of `{path, request, name, state}` objects, with the states `ok`, `orphaned` and `unreferenced_name`.

`rustman snapshot prune [path]` finds the orphaned and unreferenced snapshot files and lists them as `obsolete <path> (<state>)`. Deleting tracked files is opt-in:

- `--yes` (`-y`) deletes them and lists each as `deleted <path> (<state>)`; a `__snapshots__/` folder left empty is removed too.
- On a terminal without `--yes`, it asks `delete 2 snapshot files? [y/N]`.
- Without a terminal and without `--yes`, it only lists them and prints `pass --yes to delete them` on stderr.
- `--dry-run` only lists them, even with `--yes`.

A file that another writer (a run with `-u`, or an approval in the TUI) changed after it was listed is kept, not deleted.

Both commands exit 0, or 2 when the workspace or a file cannot be read or deleted.
