# OpenAPI exploration

Given an OpenAPI 3.0 or 3.1 document and a target, Rustman investigates the backend on its own: it discovers which operations produce the values other operations need, plans probes for conformance, robustness and security, sends them under the [safety policy](safety.md), and reports what it finds.

An exploration runs four phases in order: **discover** (the resource graph), **plan** (the probes), **execute** (sending them) and **report**. `rustman explore plan` stops after planning and sends nothing; `rustman explore run` goes all the way. This page covers both, dependency discovery, the probe families, the finding rules and their severities, reproducing findings, interrupting and resuming, and the `[explore]` table. The report formats (JSON, JUnit, SARIF, Markdown, HTML), `rustman report render` and `report diff` are described in [Reports](reports.md).

## Planning: `rustman explore plan`

`rustman explore plan` discovers the resource graph and plans every probe, and sends nothing to the target. It prints each planned probe with its family, operation, method, expected outcome, and whether it is allowed, blocked by the safety policy (and why) or skipped (and why):

```text
$ rustman explore plan --spec api.yaml --target https://staging.example.com --seed 1234 \
    --probes conformance,auth,lifecycle --operations '*User*'
plan: 6 probes (2 allowed, 4 blocked, 0 skipped), seed 1234

ID     FAMILY       OPERATION           METHOD  EXPECTED                                   STATUS
p0001  conformance  GET /users          GET     documented status and schema               allowed
p0002  conformance  POST /users         POST    documented status and schema               blocked: mutations not allowed
p0003  conformance  GET /users/{id}     GET     documented status and schema               allowed
p0004  conformance  PUT /users/{id}     PUT     documented status and schema               blocked: mutations not allowed
p0005  conformance  DELETE /users/{id}  DELETE  documented status and schema               blocked: destructive not allowed
p0006  lifecycle    GET /users/{id}     GET     update reflected, 404 or 410 after delete  blocked: mutations not allowed
```

| Flag | Meaning |
|------|---------|
| `--spec <file\|url>` | The OpenAPI document (read like [`rustman import openapi`](import-openapi.md) reads it; `--allow-remote-refs` too) |
| `--target <url>` | The base URL probes are sent to; its origin is the only one allowed |
| `--probes <list>` | Comma-separated [families](#probe-families) (default: all but `bola`) |
| `--operations <glob>` | Only operations whose `operationId`, path or `METHOD /path` matches (`*` and `?`; repeatable) |
| `--seed <u64>` | The seed of the fuzz cases; random when left out, and always printed |
| `--fuzz-cases <n>` | At most this many fuzz cases per operation (default 50) |
| `--allow-mutations`, `--allow-destructive`, `--allow-host`, `--production` | The [safety flags](safety.md) |
| `--identity-b <env>` | The environment of a second identity, for `bola` probes |
| `--env`, `--workspace` | The active environment and the workspace (a workspace is optional for `plan`) |
| `--graph` | Also print the resource graph |
| `--json` | Print the plan as JSON: `seed`, `probes` (each with its `steps`) and `scans`, plus `graph` with `--graph` |

The plan is a pure function of the document, the graph, the seed and the settings: the same seed gives the identical plan. Probes refused by the safety policy are listed as `blocked: <reason>`, never dropped. A plan against a production target is refused with exit code 4 unless `--production` repeats its host, as for any governed sender.

Probe requests are templates: a path parameter whose value is in the active environment is sent as `{{name}}`, and values captured from earlier steps the same way. Plans never contain variable values, so secrets can't leak into them.

## Running: `rustman explore run`

`rustman explore run` takes every flag of `explore plan` (but `--graph` and `--json`), plus the budgets and the failure threshold:

| Flag | Meaning |
|------|---------|
| `--rate <n>` | Requests per second at the start (default: `[safety].rate`, 10); halved on `429` |
| `--concurrency <n>` | Probes in flight at once (default: `[safety].concurrency`, 4) |
| `--max-probes <n>` | Probe budget: at most this many requests (default 2000) |
| `--max-duration <duration>` | Time budget: `90s`, `15m`, or milliseconds (default `15m`) |
| `--fail-on <severity>` | Exit 1 when a finding is at or above it: `critical`, `high` (default), `medium`, `low` or `info` |
| `--viz` | Show the exploration live in the [viz companion](cli.md#viz-companion): probes, findings and coverage as they come. Without one it continues without visuals, with the same exit code. Also on `explore resume`. |

```text
$ rustman explore run --spec api.yaml --target https://staging.example.com --env staging --allow-mutations
discover: 9 operations, 7 producer edges, 1 unresolved parameters
plan: 112 probes (104 allowed, 6 blocked, 2 skipped), seed 8327461120938471
exploration exp-20261118-3fa9c1 started (41 ms to discover and plan)
execute: 37/112 probes, 4 findings, 1.0s
execute: 112/112 probes, 9 findings, 3.2s
exploration exp-20261118-3fa9c1 completed (seed 8327461120938471)
probes: 112 (71 passed, 31 finding, 6 blocked, 2 skipped, 0 not run, 2 failed)
findings: 1 critical, 2 high, 4 medium, 1 low, 1 info
  f001  critical  auth-not-enforced  GET /admin/users  (1x)
  f002  high      server-error-on-invalid-input  POST /users  (12x)
  ...
coverage: operations 8/9 (88.9%), statuses 11/17 (64.7%)
left behind: 3 resources
  https://staging.example.com/users/41
  https://staging.example.com/users/42
  https://staging.example.com/users/43
report: .rustman/explorations/exp-20261118-3fa9c1/report.json
```

Phases and progress go to stderr (at most one progress line a second); the summary goes to stdout, or to stderr when a `--report` writes to stdout. `--report <format>[=<path>]` (repeatable) also renders the finished report as `json`, `junit`, `markdown`, `html` or `sarif`, as in `--report html=out/r.html --report sarif=out/r.sarif`; see [Reports](reports.md).

Probes run under the [safety policy](safety.md): read-only by default, `--allow-mutations` for `POST`, `PUT` and `PATCH`, `--allow-destructive` for `DELETE`, the target's origin only (plus `--allow-host` and `[safety].allowed_hosts`), and the [production guard](safety.md#production-guard): against a production target the run is refused with exit code 4 unless `--production <host>` repeats the target host, and `--allow-destructive` is never allowed there. Probes the policy refuses are reported as `blocked: <reason>`, and probes left when a budget runs out as `not run: budget`.

### The exploration directory

Each exploration gets an id, `exp-<YYYYMMDD>-<6 hex digits>`, and a directory in the workspace (or, outside one, in the current directory):

```text
.rustman/explorations/exp-20261118-3fa9c1/
  state.json      the phase, progress, settings, document hash and resource graph
  plan.json       the planned probes
  journal.ndjson  one line per finished probe and per cleanup, synced as written
  report.json     the report, once every probe has run
```

`.rustman/` is ignored by Git (`rustman init` adds it to `.gitignore`). Every value in these files is [redacted](environments.md#redaction) as it is written.

### Cleanup and resources left behind

A step that creates a resource (a `POST` answering 2xx) is identified by the id in its response: the field the [resource graph](#dependency-discovery) gives for the path parameter of `<path>/{param}` (`/users/{id}` for `POST /users`). Each one is recorded in the journal with its URL. A lifecycle chain's own `DELETE` removes it from the list.

When the probes are done, or after an interrupt, the resources still there are deleted newest first with their documented `DELETE` operation, if `--allow-destructive` was given. These deletes go through the safety policy and the rate limit like probes, but the probe and time budgets neither count nor stop them, so the cleanup still runs when a budget ran out. Otherwise (and when a delete fails) they are listed in the report under `left_behind` and in the summary as `left behind`.

## Status

`rustman explore status [<id>]` shows an exploration, the most recent one by default, while it runs or after it ended:

```text
$ rustman explore status exp-20261118-3fa9c1
exploration exp-20261118-3fa9c1 (running)
phase: execute
probes: 37/112 done
findings: 1 critical, 1 high, 2 medium, 0 low, 0 info
elapsed: 1.4s
target: https://staging.example.com
started: 2026-11-18T10:02:11.482Z
```

The state is `running`, `interrupted` or `completed`. `--json` prints the same fields as JSON (`id`, `state`, `phase`, `done`, `total`, `findings`, `elapsed_ms`, `target`, `started_at`, `updated_at` and, once written, `report`).

## Interrupting and resuming

On Ctrl+C, `explore run` stops dispatching probes, waits up to 10 seconds for the probes in flight, runs the cleanup (as above), saves its state and exits with code 130. A second Ctrl+C quits at once. A probe still in flight after the 10 seconds is reported as `not run: interrupted` and runs again on resume; the resources it had already created are journaled, so the cleanup deletes them (or lists them as left behind).

`rustman explore resume <id>` continues it: it re-reads the document and refuses (exit code 2) if its SHA-256 changed since the exploration started, then runs only the probes the journal does not hold, with the same plan, seed, safety flags and budgets (minus what earlier runs used), and writes the report over all of them. A lifecycle chain is one probe, so a chain cut short runs again from its start. The environment is the one the exploration started with.

## Findings and the report

Findings on the same rule and operation are reported once, with the number of `occurrences`, the probes they came from, and the first occurrence's message and redacted evidence (the request, the response status and headers, and the first 4 KiB of the body). They are numbered `f001`, `f002`, ... from the most severe.

`report.json` is a `rustman.report/v1` document with `"kind": "exploration"`. Besides `id`, `started_at`, `finished_at`, `environment`, `target`, `spec`, `document_sha256` and the `seed`, it holds:

| Field | Content |
|-------|---------|
| `settings` | The probe selection, the safety flags and the budgets |
| `summary.probes` | Probe totals by outcome: `passed`, `finding`, `blocked`, `skipped`, `not_run`, `failed` (a request error or a chain that stopped early), and `total` |
| `summary.findings` | Distinct findings by severity |
| `coverage` | `operations` exercised and documented `statuses` observed (`covered`, `total`, `percent`), and the `matrix`: each operation's documented statuses marked `observed`, `not-observed` or `blocked` (nothing was sent for the operation because its probes were blocked) |
| `findings` | The findings, as above, each with its `remediation` and `reproduction` |
| `probes` | Every probe with its `outcome` and, unless it passed, the `reason` (`blocked: ...`, `skipped: no producer`, `not run: budget`, `not run: interrupted`, or why it failed) |
| `left_behind` | Created resources that were not deleted |
| `cleanup` | Each delete of a created resource and the status it got |
| `operations` | The documented operations in document order, with their `operation_id` and `tags` (JUnit groups by the first tag) |

## Reproducing findings

Every finding carries a `reproduction`: its evidence request as a curl command, with secrets left as `{{name}}` references, and a request file that sends it to `{{baseUrl}}` and asserts what a correct API answers. `rustman explore export <id> --findings <ids|all> --into <dir>` writes those request files, one per finding (`f001-auth-not-enforced-get-admin-users.rman.yaml`):

```yaml
version: 1
kind: http
name: 'auth-not-enforced: GET /admin/users'
method: GET
url: '{{baseUrl}}/admin/users'
auth:
  type: none
assertions:
  - status:
      - 401
      - 403
```

Run them as a regression suite with the environment of an imported collection, or `--var baseUrl=<url>`: `rustman run regressions/ --env staging`. They fail while the bug is there and pass once it is fixed. The assertion per rule:

| Rule | Assertion |
|------|-----------|
| `auth-not-enforced`, `unexpected-auth-status` | `status: [401, 403]` |
| `broken-object-level-authorization` | `status: [401, 403, 404]` |
| `server-error-on-invalid-input`, `invalid-input-accepted` | `status: 4xx` |
| `undocumented-method-allowed` | `status: [404, 405]` |
| `resource-survives-delete` | `status: [404, 410]` |
| `server-error`, `undocumented-status` | the documented status codes |
| `response-schema-violation` | the status, and the body against the documented schema |
| `update-not-reflected` | each updated field equals its new value |
| `error-leakage` | the body no longer contains the leaked excerpt |
| `missing-hsts` | `Strict-Transport-Security` is present |
| `missing-content-type-options` | `X-Content-Type-Options` equals `nosniff` |
| `permissive-cors` | `Access-Control-Allow-Credentials` is absent |

A finding from a chain (lifecycle, BOLA) exports its last request with the ids it captured, so it only reproduces while those resources exist.

## Exit codes

`explore run` and `explore resume` exit with:

| Code | Meaning |
|------|---------|
| 0 | No finding at or above `--fail-on` |
| 1 | At least one finding at or above `--fail-on` (default `high`) |
| 2 | Invalid input or configuration: a bad flag, an unreadable or invalid document, an unknown exploration, a changed document on resume |
| 4 | Refused by the safety policy (the production guard) before anything was sent |
| 130 | Interrupted by Ctrl+C |

## Dependency discovery

Many operations need a real id: `GET /users/{id}` is only meaningful with the id of a user that exists. Discovery finds, for each path parameter, the operations that produce its value, best first:

1. **Overrides** from `[explore.producers]` in `rustman.toml`.
2. **OpenAPI links**: a success response's `links` naming the operation (by `operationId` or `operationRef`) with a `$response.body#/<pointer>` value for the parameter.
3. **The heuristic**: an operation under a prefix of the consumer's path whose success response has a field named like the parameter, or `id` when the parameter is `id` or `<resource>Id` (`orderId` for `/orders`). POSTs come first, then the longest prefix. A list response (an array of objects) produces its first item's field.

`--graph` prints one line per edge, `consumer: parameter <- producer pointer (source)`, then the parameters nothing produces:

```text
GET /users/{id}: id <- POST /users /id (heuristic)
GET /users/{id}: id <- GET /users /0/id (heuristic)
GET /orders/{orderId}: orderId <- POST /orders /id (heuristic)
GET /reports/{reportId}: reportId <- no producer
```

A probe that needs a real value runs its producer first, as a setup step, and passes the captured value on. The first producer the safety policy allows is used, so a read-only plan reads ids from a list operation when there is one. Probes that need a parameter with no producer and no value in the active environment are planned as `skipped: no producer`.

When the heuristic gets it wrong, pin the producer per parameter name, as an `operationId` or `METHOD /path`, optionally with a JSON pointer into its response body after `#`:

```toml
[explore.producers]
orderId = "createOrder#/id"
slug = "POST /teams#/handle"
```

## Probe families

| Family | What it sends | Expected |
|--------|---------------|----------|
| `conformance` | One valid request per operation | A documented status, and a body that matches its schema |
| `auth` | For operations that require credentials: no credentials, then a well-formed invalid one | 401 or 403 |
| `negative-input` | Requests that omit required fields or parameters, use wrong types, or break bounds, length, pattern, enum or item counts (each variant is checked to be invalid against its schema; at most 20 per operation) | 4xx |
| `method-not-allowed` | One undocumented method per documented path (from GET/HEAD/OPTIONS when read-only) | 405 or 404 |
| `lifecycle` | create, read, update, read, delete, read along the graph, passing the created id; steps the policy refuses after the create are left out | The update is reflected; 404 or 410 after the delete |
| `bola` | Off by default. Identity A reads a resource, then identity B (`--identity-b`) reads it by id; skipped without a second identity | B is refused |
| `fuzz` | Seeded cases per operation with a JSON body or query parameters, alternating schema-shaped values and values on the bounds (minimum/maximum, shortest/longest) | No 5xx; a documented status and schema |
| `undocumented-status` | Nothing of its own: checks every response | A documented status |
| `error-leakage` | Nothing of its own: scans every response body (first 64 KiB) | No stack traces, SQL errors, debug pages or internal paths |
| `security-headers` | Nothing of its own: checks every response | `X-Content-Type-Options: nosniff`; HSTS over HTTPS; no `*` CORS with credentials |

Fuzz values come from `proptest` strategies built from each schema (types, formats, bounds, `pattern`, `enum`, required properties) and a ChaCha generator seeded from `--seed` and the operation, so a seed reproduces every case.

## Finding rules

Every finding has a stable rule id and a default severity:

| Rule | Severity | Raised when |
|------|----------|-------------|
| `auth-not-enforced` | critical | A request without valid credentials got 2xx |
| `broken-object-level-authorization` | critical | Identity B read identity A's resource |
| `server-error-on-invalid-input` | high | Invalid input got 5xx |
| `server-error` | high | A valid request got an undocumented 5xx |
| `response-schema-violation` | medium | A body breaks the schema of its status and media type (the first violation, with its instance and schema pointers) |
| `invalid-input-accepted` | medium | Invalid input got 2xx |
| `undocumented-method-allowed` | medium | An undocumented method got 2xx |
| `error-leakage` | medium | A body reveals internals (with the matched excerpt) |
| `update-not-reflected` | medium | A read after an update shows other values |
| `resource-survives-delete` | medium | A read after a delete did not get 404 or 410 |
| `unexpected-auth-status` | low | A request without valid credentials was refused with neither 401 nor 403 |
| `missing-hsts` | low | An HTTPS response has no `Strict-Transport-Security` |
| `missing-content-type-options` | low | A response has no `X-Content-Type-Options: nosniff` |
| `permissive-cors` | low | `Access-Control-Allow-Origin: *` with `Access-Control-Allow-Credentials: true` |
| `undocumented-status` | info | A status is not documented for its operation |

Findings carry the redacted evidence exchange: the request (URL, headers, body), the response status and headers, and the first 4 KiB of the body. Secret values and sensitive headers are replaced as everywhere else (see [Environments](environments.md)). Every secret of both identities (`--env` and `--identity-b`) is redacted from all evidence, including responses to requests that did not send it, such as a no-credentials auth probe. A credential kept in a plain environment variable instead of a secret (a `bearerAuth_token` under `variables:`) is redacted the same way, the request headers of the evidence include those the auth adds (`Authorization: Bearer «redacted:bearerAuth_token»`), and the curl reproduction keeps every credential as its `{{name}}` reference, whatever its source. `X-Api-Key` and the header of an `apiKey` scheme are treated like `Authorization`. A finding is filed under the operation of the step whose response it is about: what a setup step (such as the `GET /users` that finds an id for `GET /users/{id}`) answered counts for that step's operation.

Rules can be turned off per workspace:

```toml
[explore]
disabled_rules = ["missing-content-type-options"]
```

## For developers

Exploration lives in `rustman-explore` (`rustman_explore::explore`), a library the CLI only wires up, and that other automated senders (the mock server, stress tests) build on:

- `graph::Graph::build(&model, &producers)` discovers the graph.
- `plan::plan(&model, &graph, &PlanOptions)` returns the `Plan`, a `Vec<Probe>` of steps.
- `exec::Executor` runs one probe at a time through a `rustman_http::GovernedSender`, the only sender it can hold, and returns its `Outcome`: what was sent, the findings (with their reproduction) and the resources created or deleted.
- `runner::execute` runs the probes a journal does not hold, `concurrency` at a time, journals each outcome (`journal::Journal`), stops on a `CancellationToken` after draining, and cleans up.
- `session::Session` is the exploration directory and `state.json`; `drive::drive` runs the execute and report phases on it, and `report::build` turns a plan and a journal into the `ExplorationReport`.
- `detect` holds the detectors, `rules::RULES` the registry, `repro` the reproductions and `export` the regression files.

The `bad-api` server in `rustman-test-support` (`rustman_test_support::bad_api`) is a deliberately broken API with its document, used by the exploration tests; `BadApi::start_fixed()` serves it with every bug fixed.
