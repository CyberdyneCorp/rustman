# Contract testing

`rustman contract` checks that an API keeps its promises to its clients, in three ways:

- `rustman contract diff` compares two versions of an OpenAPI document, decides which differences fail the build, accepts known ones in an allowlist and reads the old version from Git. The rules it classifies with are listed in the [contract rule catalog](contract-rules.md).
- `rustman contract pact generate` and `rustman contract pact verify` write [Pact](https://docs.pact.io/) contracts from a collection (the consumer side) and replay them against a provider (the provider side).
- `rustman contract verify` checks recorded traffic (history or a run report) against an OpenAPI document, without sending any request.

Every command writes the same kind of report (`kind: contract`), and every output goes through the active environment's redaction.

## Comparing two documents

```text
rustman contract diff <OLD> <NEW> [--fail-on-unknown] [--fail-on <breaking|unknown|non-breaking>]
                      [--allowlist <FILE>] [--report <FORMAT>[=<PATH>]]... [--env <NAME>] [--workspace <DIR>]
rustman contract diff --base <GIT-REF> <PATH> [same options]
```

`OLD` and `NEW` are files or `http(s)` URLs, loaded and validated as [OpenAPI import](import-openapi.md) does. An invalid document prints the import diagnostics and exits 2. Every difference is printed with its classification, rule id, operation, a message and a JSON pointer into each document, followed by the counts:

```text
breaking      operation-removed  DELETE /users/{id}: operation removed
  old: /paths/~1users~1{id}/delete
  new: /paths
non-breaking  operation-added    GET /health: operation added
  old: /paths
  new: /paths/~1health/get

1 breaking, 1 non-breaking, 0 unknown (rule catalog version 2)
```

## Exit codes and strictness

| Exit code | When |
|-----------|------|
| `0` | No difference fails the diff |
| `1` | At least one difference fails it: by default, any `breaking` difference the allowlist does not accept |
| `2` | An invalid document or allowlist, an unknown flag value, or a `--base` ref or path that cannot be read |

`unknown` differences (such as `composition-changed`) do not fail the diff unless `--fail-on-unknown` (or `--fail-on unknown`) is given. `--fail-on non-breaking` makes every difference fail, for frozen APIs. Allowlisted differences never fail.

## The allowlist

`contracts/allowlist.yaml`, in the workspace (or in the current folder outside a workspace), lists the differences a team has accepted; `--allowlist <FILE>` reads another file instead. Like Pact files and run reports, an allowlist larger than 20 MiB is refused (exit 2). Each entry names a `rule` and an `operation` (`METHOD /path`, as the diff prints it), optionally a `pointer` (into either document) to accept one element only, and needs a non-empty `justification`. `expires` (`YYYY-MM-DD`) ends the acceptance after that day.

```yaml
- rule: operation-removed
  operation: "DELETE /users/{id}"
  justification: Deprecated since 2026-06, no traffic
  expires: 2026-12-31
- rule: response-enum-widened
  operation: GET /pets
  pointer: /components/schemas/Pet/properties/status
  justification: Every client maps unknown statuses to "other"
```

An accepted difference is still reported, as `breaking (allowed)`, with its justification. The file is checked before the documents are compared; every invalid entry is reported with its line and exits 2:

```text
error: contracts/allowlist.yaml:4: justification is required
```

Unknown rule ids, a missing `operation`, an `expires` that is not a date and unknown keys are errors too. Two situations only warn, on stderr, and leave the exit code alone:

- `warning: contracts/allowlist.yaml:1: allowlist entry expired on 2026-01-01`: the entry allows nothing, so the difference it matched fails again.
- `warning: contracts/allowlist.yaml:5: unused allowlist entry (operation-removed DELETE /users/{id})`: nothing matched it, and it can be removed.

## Comparing with a Git ref

```sh
rustman contract diff --base origin/main openapi.yaml
```

reads the old document from `origin/main:openapi.yaml` in the repository that contains the current folder, and the new one from the working tree. Relative `$ref` files are read at the same ref, so a schema moved to another file is compared as it was. Paths are relative to the current folder. Rustman runs `git show <ref>:<path>`, so `git` must be on the `PATH`.

A path that does not exist at the ref exits 2 with `openapi.yaml not found at origin/main` (a document added on the branch has nothing to compare with). A ref that does not exist exits 2 too; in a shallow CI clone, fetch it first:

```sh
git fetch origin main --depth=1
rustman contract diff --base origin/main openapi.yaml --report markdown=out/contract.md
```

## Reports

`--report <format>[=<path>]` (repeatable) writes the diff as a `rustman.report/v1` report with `kind: contract` and `mode: diff` (see [Reports](reports.md#contract-reports)), as `json`, `markdown`, `junit`, `sarif` or `html`. A report without a path goes to stdout, and the human summary then goes to stderr. The Markdown report suits a pull request comment: a summary line with the counts per classification, then a table per classification, breaking differences first, each with its rule id, operation, message and pointer.

## In a GitHub Actions pull request workflow

The diff needs the base branch in the clone: check out with `fetch-depth: 0` (or fetch the base branch), then compare the document with the base. The Markdown report suits a pull request comment, and the SARIF report shows each difference on the changed line of the pull request.

```yaml
# .github/workflows/contract.yml
name: API contract
on: pull_request
permissions:
  contents: read
  security-events: write
jobs:
  contract:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
        with:
          fetch-depth: 0
      - name: Install rustman
        run: cargo install --git https://github.com/CyberdyneCorp/rustman rustman-cli --locked
      - name: Breaking changes
        run: rustman contract diff --base origin/${{ github.base_ref }} openapi.yaml --report markdown=out/contract.md --report sarif=out/contract.sarif
      - uses: github/codeql-action/upload-sarif@v3
        if: always()
        with:
          sarif_file: out/contract.sarif
```

The step fails (exit 1) on a breaking difference that `contracts/allowlist.yaml` does not accept; `out/contract.md` can be posted as a comment by a later step.

## Pact contracts

### Generating a Pact file (consumer side)

```text
rustman contract pact generate [<COLLECTION>...] --consumer <NAME> --provider <NAME>
                               [--pact-version <3|4>] [--output <FILE>] [--env <NAME>] [--workspace <DIR>]
```

writes one Pact file for the consumer and provider pair, by default `contracts/pacts/<consumer>-<provider>.json` in the workspace (meant to be committed, or published with the `pact-broker` CLI). `COLLECTION` names folders or request files (the whole workspace by default); every `http` request in them becomes one interaction, named after the request. Requests of other kinds are skipped with a warning.

The request comes from the request file with its variables resolved in the active environment (an unresolved variable stops generation with exit 2). A secret is never written: its place holds `«redacted:<name>»`, and the value gets a `regex` matcher, so `Authorization: Bearer {{token}}` becomes the example `Bearer «redacted:token»` with the matcher `^Bearer .+$`. Dynamic values (`{{$uuid}}`, `{{$timestamp}}`, ...) are written as `«dynamic:$uuid»` with a matcher the same way, so a file generated twice is the same file.

The expected response comes from the assertions:

| Assertion | Pact |
|-----------|------|
| `status: 200` | response status 200 |
| `status: 2xx` | v4: a `statusCode` matcher (`success`); v3: the status last recorded in history for the request, else 200 with a warning |
| `status: [200, 201]` | v4: a `statusCode` matcher with the codes; v3: the first code |
| `header: {name, equals}` / `{matches}` / `{contains}` | the header with an `equality` / `regex` / `include` matcher |
| `json: {path, equals}` | the value at the path, with an `equality` matcher |
| `json: {path, type}` | an example of the type, with a `type` matcher (`integer` for integers) |
| `json: {path, matches}` / `{contains}` | a `regex` / `include` matcher |
| `json: {path, length}` | an empty example array, with a `type` matcher whose `min` and `max` are the length (see [Interop](#interop-with-the-official-verifier)) |

A Pact `regex` matcher must match the whole value, while a Rustman `matches` finds the pattern anywhere, so a pattern that is not anchored at both ends is written as `[\s\S]*(?:<pattern>)[\s\S]*` (`matches: "^A"` becomes `[\s\S]*(?:^A)[\s\S]*`); `^\d+$` is kept as it is.

Other assertions (`latency`, `schema`, `body`, the `json` comparisons, filter or recursive JSON paths) have no faithful Pact equivalent; each is left out with a warning that names it and the request file, and the interaction is still written:

```text
warning: latency assertion has no Pact equivalent (users/get-user.rman.yaml)
```

Files follow the Pact Specification v4 by default (`metadata.pactSpecification.version` is `4.0`); `--pact-version 3` writes v3 (`3.0.0`).

### Verifying a provider

```text
rustman contract pact verify <PACT_FILE>... --provider-url <URL> [--provider-states-url <URL>]
                             [--allow-mutations] [--allow-destructive] [--production <HOST>]
                             [--report <FORMAT>[=<PATH>]]... [--env <NAME>] [--workspace <DIR>]
```

replays every interaction against the provider and compares the status, the expected headers and the body under the interaction's matching rules (`type`, `regex`, `equality`, `include`, `integer`, `decimal`, `number`, `min`/`max`, `eachLike`, `statusCode`). A `regex` must match the whole value, as in Pact (`[a-z]+` does not match `ab-1`). An interaction whose request path does not start with `/` (such as `@other-host/x`, which would change the host once appended to the provider URL) fails with `request path '...' does not start with '/'` and nothing is sent for it. Every mismatch names the interaction and where it is, as a JSON path for the body:

```text
failed   get user (contracts/pacts/web-users.json)
  $.id: expected number 42, got string "42"

0 passed, 1 failed, 0 blocked
```

An interaction with provider states needs `--provider-states-url`: before the interaction, each state is POSTed to it as `{"consumer", "state", "params", "action": "setup"}`, and after it with `"action": "teardown"`. A non-2xx setup fails the interaction. Stateful interactions without the URL stop verification before anything is sent (`provider states declared but --provider-states-url not set`, exit 2).

Every request goes through the [safety policy](safety.md): only the provider URL's and the state URL's origins may be reached, and `[safety]` paces them. Interactions that send `POST`, `PUT` or `PATCH` need `--allow-mutations`, and `DELETE` needs `--allow-destructive`; without them the interaction is reported as `blocked: mutations not allowed` (or `blocked: destructive not allowed`) and nothing is sent. State calls are POSTs to the endpoint named for them, so they need no flag. A provider whose host is in `[safety].production_hosts` is refused (exit 4) unless `--production <HOST>` repeats that host. Requests are sent as the Pact file holds them: a secret header holds its `«redacted:...»` example, so a provider that checks credentials needs a provider state (or a verification environment) that accepts it.

A typical flow, with the provider running locally:

```sh
rustman contract pact generate users --consumer web --provider users --env dev
rustman contract pact verify contracts/pacts/web-users.json --provider-url http://localhost:8080 --provider-states-url http://localhost:8080/_pact/state
```

| Exit code | When |
|-----------|------|
| `0` | Every interaction passed |
| `1` | At least one interaction failed |
| `2` | An unreadable Pact file (or one over 20 MiB, the limit OpenAPI documents have too), stateful interactions without `--provider-states-url`, or an invalid URL |
| `4` | At least one interaction was blocked by the safety policy, or the provider is a production host (see [Safety](safety.md)) |

The Pact files Rustman writes validate against the published Pact JSON schemas, and a CI job verifies them with the official verifier (`pact_verifier_cli`) too, expecting the same verdicts.

### Interop with the official verifier

The `pact-interop` CI job generates a v3 and a v4 Pact file, verifies both with Rustman and with `pact_verifier_cli` 1.4.0 against the same fixture provider, and requires the same verdict for every interaction but one, which the implementations disagree on:

- **`length` on an array.** `json: {path: "$.tags", length: 2}` is written as the example `"tags": []` with `{"match": "type", "min": 2, "max": 2}`. The example is empty because the assertion says nothing about the items, and any example item would constrain their type: the `type` matcher cascades to the items. The specification's matcher tables (v3 and v4, `MinType`/`MaxType`/`MinMaxType`) say that for a collection "the length of the actual value is compared against the minimum" and maximum, and Rustman does that, so `"tags": []` fails. `pact_matching`, which `pact_verifier_cli` uses, applies no matcher to a list whose example is empty (`compare_lists_with_matchingrule` in `pact_matching/src/matchingrules.rs`), so it passes any array there, of any length. The same holds in v3 and v4. A provider verified with the official tools is therefore not held to a `length` assertion; one verified with Rustman is.

## Passive conformance

```text
rustman contract verify --spec <OPENAPI> [--from <history|RUN_REPORT>] [--since <AGE>] [--grep <TEXT>] [--limit <N>]
                        [--fail-on-undocumented] [--report <FORMAT>[=<PATH>]]... [--env <NAME>] [--workspace <DIR>]
```

checks recorded exchanges against a document and opens no network connection. The document must be a local file: a URL is refused before anything is sent (download it first), and so are `$ref`s to remote documents. The exchanges come from the workspace [history](history.md) (the ad-hoc history outside a workspace), filtered with `--since`, `--grep` and `--limit` as `rustman history list` does, or from a run report written by `rustman run --report json=<path>` (`--from <path>`). Each exchange is matched to an operation by method and path template, its request is checked against the operation's parameters and body schema, and its response against the documented status and the JSON schema of its body:

```text
violation     GET /users/{id}: exchange 01JD3W6Q0Z5RKM8V9S4T2B7XNC (GET http://localhost:8080/users/7)
  $.email: required property missing
undocumented  GET /internal/debug: exchange 01JD3W6Q1C8D2F4G6H8J0K2M4P (GET http://localhost:8080/internal/debug)

observed operations:
  GET /users/{id}  3 exchanges, 1 with violations
  GET /users  2 exchanges, 0 with violations

2/12 operations observed; 1 violation, 1 undocumented, 4 conforms
```

An exchange that matches no operation is `undocumented`. The summary lists the documented operations the traffic exercised with their number of exchanges, and the coverage (`5/12 operations observed`); the report carries the same coverage, with violations per operation.

```sh
rustman run users --env dev --report json=out/run.json
rustman contract verify --spec openapi.yaml --since 7d --report markdown=out/conformance.md
rustman contract verify --spec openapi.yaml --from out/run.json --fail-on-undocumented
```

| Exit code | When |
|-----------|------|
| `0` | No violation (and, with `--fail-on-undocumented`, no undocumented exchange) |
| `1` | At least one violation, or an undocumented exchange with `--fail-on-undocumented` |
| `2` | An invalid document, an unreadable run report or a report of another kind |

## Secrets

Documents can hold anything, such as an example with a real token, and a provider or a recorded response can echo a secret back. With a workspace and an active environment (`--env`, `RUSTMAN_ENV` or `default_environment`), every secret value of that environment is replaced with `«redacted:<name>»` in everything a contract command prints or writes: the diff, Pact files, verification mismatches, conformance evidence, the warnings and every report. Secrets used by a request are never written to a Pact file, whatever the environment (see [Generating a Pact file](#generating-a-pact-file-consumer-side)).
