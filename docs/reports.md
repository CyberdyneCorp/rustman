# Reports

`rustman run` and `rustman explore run` describe what they did in one canonical, versioned report: a JSON document under the schema `rustman.report/v1`. Every other format is rendered from that document, for every audience: machines (JSON), CI test tabs (JUnit XML), code-scanning dashboards (SARIF 2.1.0) and people (Markdown and a self-contained HTML page). No format ever shows a secret.

## The canonical report

A report has a top-level `schema` (`rustman.report/v1`) and a `kind`:

| Kind | Written by | Content |
|------|------------|---------|
| `run` | `rustman run --reporter json`, `rustman send` | Every request with its file, redacted exchange, timings, assertion results and captures; see [the CLI reference](cli.md#rustman-run) |
| `exploration` | `rustman explore run` (always, as `.rustman/explorations/<id>/report.json`) | The seed, safety settings, probe totals by outcome, the coverage matrix, the findings with evidence and reproduction, and the resources left behind; see [Exploration](exploration.md#findings-and-the-report). `rustman evm probe` writes one with `subject: evm-rpc`: its coverage matrix is methods × providers, and an `evm` section holds each provider's support, archive state, latency, subscriptions, rate limit, drift, blocks and trace; see [Probing EVM providers](evm-probe.md#reports-and-exit-codes) |
| `contract` | `rustman contract diff --report json` | What a contract command checked, as one list of items, with a `mode` (`diff`, `pact-verify` or `conformance`); see [Contract reports](#contract-reports) |
| `bench` | `rustman bench --report json=<file>` | The load profile, the target, latency and error statistics per request file and overall, a per-second time series, the threshold results and error samples; see [Bench reports](#bench-reports) |

`rustman report schema` prints the JSON Schema (draft 2020-12) that every kind validates against. The same file is published at [`schemas/report.schema.json`](schemas/report.schema.json). Its `kind: run` part is the contract the first reporters published, kept as is; its `kind: exploration`, `kind: contract`, `kind: bench` and `kind: stress` parts are generated from the Rust types with `schemars`, and a test fails if the published file drifts from them.

A reader built before a kind existed refuses it instead of misreading it: `rustman report render` exits 2 with `unknown report kind '<kind>'`.

```sh
rustman report schema > report.schema.json
```

### Versioning

Additive changes (a new optional field, a new `kind`) keep `rustman.report/v1`; reports written by older versions stay valid. A breaking change bumps the major version. `rustman report render` and `report diff` read every supported version (`rustman.report/v1` and `v1.<minor>`) and refuse any other major version with exit code 2:

```text
$ rustman report render old.json --format html
error: old.json: unsupported report schema 'rustman.report/v9' (this version reads rustman.report/v1)
```

## Rendering

```text
rustman report render <REPORT> --format <json|junit|markdown|html|sarif> [-o <FILE>] [--fail-on <severity>]
```

renders any canonical report, of either kind, to stdout or to `-o <FILE>`. `run` and `explore run` render while they finish, with `--report <format>[=<path>]` (repeatable): every rendering comes from the same report data.

```sh
rustman explore run --spec api.yaml --target http://localhost:8080 \
  --report html=out/r.html --report sarif=out/r.sarif --report junit=out/junit.xml
rustman run api --env ci --report markdown=out/run.md --report junit=out/junit.xml
rustman report render .rustman/explorations/exp-20261118-3fa9c1/report.json --format markdown -o out/r.md
```

A `--report` without a path writes to stdout; at most one report (or `--reporter`) may. Files are replaced atomically and their folders are created. When a `--report` writes to stdout, `explore run` prints its summary on stderr and `run` does not print the `pretty` report, so stdout holds one document.

SARIF locations are relative to the current directory when the OpenAPI document is under it, and rendering reads the document (when it is still there) to point each result at the operation's line.

## Formats

### JSON

The canonical report itself, pretty-printed. `report render --format json` gives back the report it read.

### JUnit XML

For a run, one `<testcase>` per request (`classname` is the request file) with `<failure>` for a failed request, `<error>` for an errored one and `<skipped>` for a skipped one.

For an exploration, one `<testsuite>` per tag (the operation's first tag; `untagged` without one, `undocumented` for methods the document does not list) and one `<testcase>` per operation. Findings at or above `--fail-on` (default: the exploration's own `--fail-on`, `high`) become a `<failure>` whose `type` is the rule id; an operation whose probes were all blocked, skipped or not run is `<skipped>`; findings below the threshold are listed in `<system-out>`.

```xml
<testsuite name="users" tests="5" skipped="1" errors="0" failures="1">
    <testcase name="POST /users" classname="users" assertions="15">
        <failure message="f002 [high] server-error-on-invalid-input: invalid input got status 500 instead of 4xx" type="server-error-on-invalid-input">f002 [high] server-error-on-invalid-input: invalid input got status 500 instead of 4xx</failure>
        <system-out>f005 [medium] error-leakage: Java stack trace: at com.example.UserService.create(UserService.java:42)</system-out>
    </testcase>
    <testcase name="DELETE /users/{id}" classname="users" assertions="2">
        <skipped message="2 probes blocked: DELETE needs --allow-destructive"/>
    </testcase>
</testsuite>
```

### SARIF 2.1.0

Each rule a finding broke is a `reportingDescriptor` (its description, its remediation as `help`, and a `security-severity` score for GitHub). Each finding is a `result`:

| Finding severity | SARIF `level` | `security-severity` |
|------------------|---------------|---------------------|
| `critical` | `error` | 9.5 |
| `high` | `error` | 8.0 |
| `medium` | `warning` | 5.5 |
| `low` | `note` | 3.0 |
| `info` | `note` | 0.0 |

The location is the OpenAPI document and the JSON pointer of the operation (the path item for an undocumented method), with the operation's line when the document can be read:

```json
{
  "ruleId": "response-schema-violation",
  "level": "warning",
  "message": { "text": "GET /users/{id}: at '': \"email\" is a required property" },
  "locations": [
    {
      "physicalLocation": {
        "artifactLocation": { "uri": "api.yaml" },
        "region": { "startLine": 52 }
      },
      "logicalLocations": [
        { "name": "GET /users/{id}", "fullyQualifiedName": "/paths/~1users~1{id}/get", "kind": "object" }
      ]
    }
  ],
  "partialFingerprints": { "rustmanFinding/v1": "response-schema-violation|GET /users/{id}" }
}
```

`partialFingerprints` keeps a finding the same alert across runs. A run report renders as SARIF too: each failed request is an `assertion-failed` result and each errored one a `request-errored` result, located in its request file.

### Markdown

A summary (target, document, seed, safety settings, probe totals and findings by severity), the findings table sorted by severity, each finding with its description, remediation, redacted evidence and curl reproduction, the coverage summary and matrix, and the resources left behind. A run renders as a summary, the requests table and the reasons of each failure. It suits pull-request comments and job summaries:

```markdown
| ID | Severity | Rule | Operation | Occurrences | Message |
|---|---|---|---|---:|---|
| f001 | critical | `auth-not-enforced` | `GET /admin/users` | 2 | a request without valid credentials got status 200 |
| f002 | high | `server-error-on-invalid-input` | `POST /users` | 1 | invalid input got status 500 instead of 4xx |
```

What comes from the target or the document is escaped: messages (which can quote a response) have every Markdown and HTML character backslash-escaped, so a response cannot inject markup or raw HTML into a viewer, and URLs and operations sit in code spans that their content cannot close. Evidence bodies sit in fences longer than any run of `~` in them.

### HTML

One self-contained file: its stylesheet and script are inlined, and it references nothing on the network (no `http://` or `https://` script, style, font or image). Findings are expandable `<details>` with the evidence and reproduction, which works with JavaScript disabled; the severity and operation filters are the only part that needs it, and they stay hidden without it. The page follows the system's light or dark theme.

### Contract reports

A `kind: contract` report has a `mode`: `diff` for [`rustman contract diff`](contract.md#reports), `pact-verify` for [`rustman contract pact verify`](contract.md#verifying-a-provider) and `conformance` for [`rustman contract verify`](contract.md#passive-conformance). It has its `sources` (for a diff, `old` and `new`, where a `--base` document is named `<ref>:<path>`; for a Pact verification, each `pact` file; for conformance, the `spec` and the `exchanges` source), the rule `catalog_version` (diffs only), a `summary` (the count per status in the mode's order, how many items an allowlist accepted, how many fail the command, and `passed`), the `items` and the `warnings` (expired and unused allowlist entries, failed provider-state teardowns). Each item has the `rule` id and its `description`, a `status`, the allowlist `allowed` justification when one accepted it, `failing`, the `subject`, a `message`, and `locations`: the primary one first, each with a JSON `pointer`.

| Mode | Statuses, in order | One item per | Subject | Primary location |
|------|--------------------|--------------|---------|------------------|
| `diff` | `breaking`, `non-breaking`, `unknown` | difference (rule id from the [catalog](contract-rules.md)) | the operation | the new document, at the changed element |
| `pact-verify` | `passed`, `failed`, `blocked` | interaction (rule `pact-interaction`) | the interaction's description and consumer | the Pact file, at `/interactions/<n>` |
| `conformance` | `violation`, `undocumented`, `conforms` | exchange (rules `conformance-violation`, `undocumented-exchange`, `conforms`) | the matched operation, or the exchange's method and path | the document, at the operation |

A conformance report also has a `coverage`: how many documented operations the exchanges `observed` out of the `total`, and every operation with its number of `exchanges` and of exchanges with `violations`.

- **Markdown** and **HTML**: the sources and catalog version, a summary line with the counts per status (`2 breaking, 1 non-breaking, 0 unknown, 1 allowed`), then one table per status in that order, breaking first and failing items first within a table, each row with the rule id, operation, message and pointer; then the allowlist justifications, the coverage of a conformance check (`1/2 operations observed` after the summary, and a table of operations) and the warnings.
- **JUnit XML**: one `<testsuite>` named after the mode (`contract diff`, `contract pact-verify`, `contract conformance`) and one `<testcase>` per item; an item that fails the command is a `<failure>` whose `type` is the rule id.
- **SARIF**: one `reportingDescriptor` per rule id and one result per item, located in the primary document (`artifactLocation`) at the JSON pointer of the changed element (`logicalLocations[].fullyQualifiedName`). An item that fails the command is at level `error`, an unaccepted `unknown` one at `warning`, the others at `note`; an allowlisted item carries its justification as an `external` suppression.

`report diff` does not compare contract reports (exit 2).

### Bench reports

A `kind: bench` report ([`rustman bench`](bench.md#reports)) has the `target`, the `environment`, the `profile` (the `executor` and its flags), `aborted` when the run stopped early (`threshold` or `interrupted`; absent otherwise), the `totals` (the latency kind `service` or `corrected`, duration, iterations completed and interrupted, scheduled and dropped iterations, requests per second and the generator's p99 lag), the `overall` statistics and those of each request file in `files` (requests, errors by class, `error_rate`, latency min/avg/p50/p90/p95/p99/max in milliseconds, assertion `checks` passed and failed, average `phases`, `bytes_sent` and `bytes_received`), the per-second `series` (requests, errors, rps, p50/p95/p99 and active VUs), the `thresholds` (each `expression`, `passed`, the `actual` value and whether it is an `abort` one), up to 20 `error_samples` per error class (class, file, redacted message and time since the start) and the `warnings`.

- **Markdown** and **HTML**: the target, executor and duration, a summary line (passed or failed, iterations, requests and rate), the thresholds as `✓ p95<300ms (212ms)` or `✗ p95<300ms (412ms)`, a latency table per file plus an `overall` row, the error samples and the warnings. The HTML page adds two inline SVG charts, latency over time (p50, p95, p99) and requests per second; it needs no script and works offline.
- **JUnit XML**: one `<testsuite>` named `bench` with one `<testcase>` per threshold, a breached one being a `<failure>` of type `threshold`, plus an `<error>` test case when the run was aborted.
- **SARIF**: one `error` result per breached threshold (rule `bench-threshold`), without a location.

`report diff` does not compare bench reports (exit 2).

### Stress reports

A `kind: stress` report ([`rustman stress`](stress.md#reports-and-the-live-view)) has its `id`, the `target`, the `environment`, the `mode`, the `profile` (the mode's `settings`, `fuzz_ratio`, `malformed` and `wire` families and the `health` request), the `seed`, the `slos`, the `overall` and per-file statistics of valid traffic, the `fuzzed` statistics apart, the per-second `series`, the `steps` (offered and achieved rate, p50/p95/p99, error rate and classes, verdict and failure mode), `breakpoint` (`max_sustainable_rps`, `breaking_rps`, `failure_mode`, `first_failing_endpoint`, why the search stopped and `max_rps`), `rate_limited_at`, `killed`, `recovery` (`status`, the time to recover, the baseline, the last answer and the timeline of health requests), the `spike`, `soak` and `wire` results, and the `findings` in the exploration format, each with its `family` and, for a crash-like finding, its `minimized` input (redacted) or `load_dependent`. A finding's `evidence` is the exchange as it happened: `request_body` is the body sent (not the minimized one), `status` and `body` the response's status and the start of its body; without a response, `status` is 0 and `body` says what happened instead.

- **Markdown** and **HTML**: the run, the breakpoint, the recovery, the step table, latency per file and the findings with their minimized inputs. The HTML page adds inline SVG charts of latency and errors against offered load, with the breakpoint marked, and the recovery timeline; it works offline.
- **JUnit XML**: one `<testsuite>` named `stress` with one `<testcase>` per finding, a `<failure>` (of the rule's type) at or above `high`.
- **SARIF**: one result per finding, located in its request file.

`report diff` compares two stress reports like explorations: findings match by rule and operation (the request file), so a `no-recovery` only in the later report is new and exits 1. A reader from before `kind: stress` refuses such a report with `unknown report kind 'stress'` (exit 2) without crashing.

## Comparing reports

```text
rustman report diff <OLD> <NEW> [--fail-on <severity>] [--format markdown|json]
```

compares two reports of the same kind:

- Findings match by rule id and operation, and are listed as `new` (only in the new report), `fixed` (only in the old one) or `unchanged`.
- For explorations, the coverage of operations and of documented statuses, old and new, with the change in percentage points.
- For runs, every assertion whose outcome changed (`pass`, `fail`, `error`, or `absent` when only one run has it), per request.

It exits 1 when a new finding is at or above `--fail-on` (default `high`), and 0 otherwise; two reports of different kinds, or an unknown schema version, exit 2.

```text
$ rustman report diff main.json branch.json
# Report diff (exploration)

1 new, 1 fixed, 2 unchanged findings.

## New

| Severity | Rule | Operation | ID |
|---|---|---|---|
| critical | `broken-object-level-authorization` | `GET /orders/{id}` | f009 |
...
```

## Redaction

No format contains the value of a secret, of a header named `Authorization`, `Proxy-Authorization`, `Cookie`, `Set-Cookie` or `X-Api-Key`, or of an auth scheme's credential: they appear as `«redacted:<name>»`, as in `Authorization: Bearer «redacted:bearerAuth_token»`. Redaction happens when the report is built, from exchanges the engine already redacted, so re-rendering a report can never recover a value. Curl reproductions keep secrets as `{{name}}` references.

## CI recipes

### GitHub code scanning

Upload the SARIF rendering so findings show up as code-scanning alerts on the OpenAPI document:

```yaml
jobs:
  explore:
    runs-on: ubuntu-latest
    permissions:
      security-events: write
    steps:
      - uses: actions/checkout@v4
      - name: Explore the staging API
        run: |
          rustman explore run --spec api.yaml --target "$STAGING_URL" --env staging \
            --report sarif=out/explore.sarif --report junit=out/explore.xml --report markdown=out/explore.md
        continue-on-error: true
      - name: Upload SARIF
        uses: github/codeql-action/upload-sarif@v3
        with:
          sarif_file: out/explore.sarif
          category: rustman-explore
      - name: Job summary
        run: cat out/explore.md >> "$GITHUB_STEP_SUMMARY"
```

Run the exploration from the repository root so the SARIF location is the document's path in the repository.

### Test results

Any CI that reads JUnit XML (GitHub test reporters, GitLab `artifacts:reports:junit`, Jenkins, Azure Pipelines) shows one test case per request of a run, or per operation of an exploration:

```yaml
test:
  script:
    - rustman run api --env ci --reporter pretty --report junit=out/junit.xml
  artifacts:
    when: always
    reports:
      junit: out/junit.xml
```

### Failing only on regressions

Keep the report of the main branch and fail a pull request only on the findings it introduces:

```sh
rustman explore run --spec api.yaml --target "$PREVIEW_URL" --report json=out/branch.json || true
rustman report diff baseline/main.json out/branch.json --fail-on high --format markdown
```

## For developers

`rustman-report` holds the canonical `Report` (`run` or `exploration`), the only input of every renderer:

- `exploration` and `run` are the report data types; `rustman-explore` builds an `ExplorationReport`, and `RunReport::new` converts a run's records.
- `Report::parse` reads any supported version; `report_schema` composes the published JSON Schema.
- `render::render` renders one `RenderFormat`, and `ReportTarget` is one `--report` value. JUnit uses `quick-junit`, SARIF is a small typed `serde` model, and Markdown and HTML are `minijinja` templates with the stylesheet and script built in via `include_str!`.
- `diff::diff` compares two reports.

Regenerate the published schema after changing the exploration types with `RUSTMAN_UPDATE_SCHEMA=1 cargo test -p rustman-report --test render`, and review format snapshots with `cargo insta review`.
