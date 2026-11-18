# CLI reference

`rustman` is the headless face of Rustman, for scripts and CI. Reports and response bodies go to stdout; warnings and errors go to stderr, prefixed with `warning:` and `error:`.

This page covers sending and running requests, reporters, terminal behavior, logging, completions and the exit codes. The other commands are described with the features they belong to.

## Commands

| Command | What it does | Described in |
|---------|--------------|--------------|
| `rustman` / `rustman tui [path]` | Open the terminal UI (`rustman` alone does so when stdin and stdout are a terminal, and prints help otherwise); `--theme`, `--ascii`, `--vim` | [Terminal UI](tui.md) |
| `rustman init [dir] [--bare]` | Create a workspace: `rustman.toml`, a `local` environment and a sample request | [Request files](request-files.md#workspace-layout) |
| `rustman env list` | List the environments and mark the active one | [Environments](environments.md#the-active-environment) |
| `rustman secret set <name> --env <env>` | Store a secret value (read from stdin or a no-echo prompt) | [Secrets](environments.md#declaring-and-setting-a-secret) |
| `rustman secret list --env <env>` | List the declared secrets and whether each is set | [Secrets](environments.md#declaring-and-setting-a-secret) |
| `rustman secret rm <name> --env <env>` | Delete a stored secret value | [Secrets](environments.md#declaring-and-setting-a-secret) |
| `rustman cookies clear` | Empty the cookie jar of an environment | [Cookies](http.md#cookies) |
| `rustman auth login <request>` | Run the OAuth2 authorization-code login for a request and cache its token | [OAuth2](http.md#oauth2) |
| `rustman import curl` | Create a request file from a curl command | [Curl import](http.md#import) |
| `rustman import <src>` / `rustman import auto <src>` | Import a collection export, detecting its format from the content | [Importing collections](import.md) |
| `rustman import postman <src>` | Import a Postman Collection v2.0 or v2.1 export | [Postman](import.md#postman-collections) |
| `rustman import postman-env <src>` | Import a Postman environment export into `environments/` | [Postman environments](import.md#postman-environments) |
| `rustman import insomnia <src>` | Import an Insomnia v4 (JSON) or v5 (YAML) export | [Insomnia](import.md#insomnia-exports) |
| `rustman import bruno <src>` | Import a Bruno collection folder or a single `.bru` file | [Bruno](import.md#bruno-collections) |
| `rustman import har <src>` | Import the requests of a HAR 1.2 capture, with `--host`, `--method`, `--content-type` and `--include-static` | [HAR](import.md#har-captures) |
| `rustman import http <src>` | Import `.http` / `.rest` files (a file or a folder) and their environment files | [HTTP files](import.md#http-files) |
| `rustman export curl <request>` | Print a request as a curl command (redacted) | [Curl export](http.md#export) |
| `rustman export postman <path>` | Write a collection folder or request file as a Postman Collection v2.1 (`-o <file>`) | [Exporting](import.md#exporting) |
| `rustman export http <path>` | Write a collection folder or request file as a `.http` file (`-o <file>`) | [Exporting](import.md#exporting) |
| `rustman send` | Send one request and print the response | [below](#rustman-send) |
| `rustman run` | Run the requests under files and folders and report the results | [below](#rustman-run) |
| `rustman history list` | List past exchanges, newest first, with `--status`, `--method`, `--grep`, `--since`, `--limit` and `--json` | [History](history.md#listing-and-searching) |
| `rustman history show <id>` | Print one exchange in full (redacted) | [History](history.md#listing-and-searching) |
| `rustman history replay <id>` | Send an exchange's request again, with secrets read from the store now | [History](history.md#replay) |
| `rustman history clear` | Delete every recorded exchange | [History](history.md#retention) |
| `rustman completions <shell>` | Print a shell completion script | [below](#completions) |

The command groups `rustman env`, `rustman secret`, `rustman cookies`, `rustman auth`, `rustman import`, `rustman export` and `rustman history` only hold the commands above: on their own they print their subcommands and exit with 2. `rustman import` also takes a source file or folder directly (`rustman import export.json`), which is the same as `rustman import auto export.json`.

`rustman --help` and `rustman <command> --help` document every flag; `rustman --version` prints `rustman <version>`, such as `rustman 0.1.0`.

### Global flags

These work before or after the command name (`rustman -v run .` or `rustman run . -v`):

| Flag | Effect |
|------|--------|
| `--color <auto\|always\|never>` | `auto` (the default) colors stdout only when it is a terminal, `NO_COLOR` is unset or empty and `TERM` is not `dumb`. `always` forces color, even when piped; `never` turns it off. |
| `-v`, `-vv`, `-vvv` | Logs on stderr: `-v` one line per request, `-vv` also the redacted request and response headers, `-vvv` everything, including the HTTP and TLS libraries. |

## rustman send

```text
rustman send <REQUEST> [--env <name>] [--var k=v]... [--timeout <duration>] [--insecure] [--body-only | -o <file>] [--show-secrets]
rustman send <METHOD> <URL> [-H 'K: V']... [-d <body> | --json <body>] [same flags]
rustman send <URL>                                    # an ad-hoc GET
```

`<REQUEST>` is a request file (relative to the current directory), its id (the path from the workspace root without `.rman.yaml`, such as `users/get-user`) or its `name`. A name shared by several requests fails with exit code 2:

```text
error: request name 'List' is ambiguous: a/list.rman.yaml, b/list.rman.yaml
```

By default the status line, the headers and the body are printed:

```text
HTTP/1.1 200
content-type: application/json
x-request-id: req-1

{"id":42,"name":"Ana","email":"ana@example.com","roles":["admin","dev"],"address":{"city":"Lisbon"}}
```

Headers and bodies are shown [redacted](environments.md#redaction): `Authorization`, `Proxy-Authorization`, `Cookie` and `Set-Cookie`, every secret the request references and every captured value that became a secret. A large body is streamed from its spill file, and a binary body (one that is not valid UTF-8) is redacted byte by byte. `--show-secrets` prints the headers and body as received instead, on the terminal only: history still stores them redacted. `--body-only` prints only the response body, redacted the same way (as received with `--show-secrets`). `-o <file>` writes the decoded body to a file, always byte for byte; use it to save an image or other binary body exactly:

```sh
rustman send get-user --env staging --body-only | jq .id
rustman send logo --env staging -o logo.png
```

The request's assertions and captures run as in `rustman run`. Failed assertions are listed on stderr (`fail: status: expected 200, got 404`) and the exit code is 1; a request that gets no response prints `error: <reason>` and exits with 3.

### Ad-hoc requests

`rustman send <METHOD> <URL>` sends a request that has no file. `-H` adds a header (repeatable), `-d` / `--data` sends a body as is and `--json` sends a JSON body with `Content-Type` and `Accept` set to `application/json`. The flags mean what they mean for curl, and the request is built the same way [`rustman import curl`](http.md) builds one.

No workspace is needed. Outside a workspace, the request is sent with the default settings, its cookies last for the command only and nothing is written to the current folder; `--env`, secrets and other environment features need a workspace:

```sh
rustman send GET https://httpbin.org/get
rustman send POST https://api.example.com/users -H 'X-Team: core' --json '{"name":"Ana"}'
```

Inside a workspace, an ad-hoc request uses the workspace settings and the active environment, so `{{base_url}}` and secrets resolve. A URL that starts with a variable is an ad-hoc URL too; `http://` is not added to it, the variable brings the scheme:

```sh
rustman send '{{base_url}}/users/42' --env dev
rustman send DELETE '{{base_url}}/users/42?key={{api_token}}' --env dev
```

## rustman run

```text
rustman run [<PATH>...] [--env <name>] [--var k=v]... [--timeout <duration>] [--insecure]
            [--bail] [--filter <tag:name | glob>]... [--iterations <n>] [--parallel <n>]
            [--reporter <format>[:<path>]]...
```

Runs every request file under the given files and folders (default: the current folder), in collection order: file-name order, or the folder's `_collection.yaml` `order:` (see [collections](request-files.md)). Paths must exist and lie inside the workspace. Values captured by one request are available to every later request of the run.

| Flag | Effect |
|------|--------|
| `--env <name>` | The active environment (default: `RUSTMAN_ENV`, then `default_environment`). |
| `--var k=v` | A variable above every other source (repeatable). |
| `--timeout <duration>` | The total timeout of every request, replacing `timeout_ms`: `500ms`, `5s`, `2m`, or a number of milliseconds. Also on `send`. |
| `--insecure`, `-k` | No TLS certificate verification; exchanges are flagged `insecure: true`. Also on `send`. |
| `--no-history` | Do not record the exchanges in [history](history.md). Also on `send`. |
| `--bail` | After the first failed or errored request, the requests not started yet are reported as `skipped (bail)`. |
| `--filter <value>` | Keep only matching requests (repeatable; a request matching any filter runs). `tag:<name>` matches the request's `tags`; anything else is a glob over the request id or file path, where `*` and `?` stay within one folder and `**` crosses folders: `users/*`, `**/login`. |
| `--iterations <n>` | Run the selected requests `n` times (default 1). Captures carry over between iterations. |
| `--parallel <n>` | Run up to `n` folders at once (default 1). |
| `--reporter <format>[:<path>]` | `pretty`, `json`, `junit` or `tap`, written to stdout or to `<path>` (repeatable; default `pretty` on stdout). At most one reporter may write to stdout. A file is replaced atomically, a symlink is followed, and a device such as `/dev/stdout` is written in place. |

A filter or path that selects no request fails with `error: no requests to run (check the paths and --filter)` (exit code 2).

```sh
rustman run api/users --env staging
rustman run . --env ci --filter tag:smoke --bail
rustman run . --env ci --parallel 4 --reporter json:out/report.json --reporter pretty
```

### Parallel folders

With `--parallel <n>`, each folder is a unit of sequential work and up to `n` units run at once, so the requests of one folder still run in order and their captures chain. A folder whose `_collection.yaml` sets `parallel: true` runs its requests concurrently instead, still up to `n` at once; with the default `--parallel 1` it runs them in order like any other folder. Reports always list requests in collection order, whatever order they finished in.

```yaml
# load/_collection.yaml
parallel: true
```

### Reporters

`pretty` prints one line per request, the reasons below each request that did not pass, and a summary:

```text
PASS  users/list  GET 200  12 ms
FAIL  users/create  POST 404  8 ms
      status: expected 201, got 404
ERROR users/slow
      timeout after 500 ms (response)
SKIP  users/delete  skipped (bail)

4 requests: 1 passed, 1 failed, 1 errored, 1 skipped (35 ms)
```

`json` writes a `rustman.report/v1` document with `"kind": "run"`, described by the JSON Schema in [`schemas/report.schema.json`](schemas/report.schema.json). It holds every request (with its redacted exchange, timings, assertion results and capture results) and `summary.total`, `summary.passed`, `summary.failed`, `summary.errored`, `summary.skipped`, `summary.duration_ms` and `summary.interrupted`. A request is `passed` when every assertion passed and every required capture matched, `failed` when an assertion failed or could not be evaluated or a required capture matched nothing, `errored` when no response arrived (unresolved variables, network, TLS, timeout, auth) and `skipped` with a `skip_reason`: `bail`, `interrupted`, or `unsupported_kind` for request kinds this build cannot send. Report files are replaced atomically.

`junit` writes JUnit XML for CI systems: one `<testsuite name="rustman">` with one `<testcase>` per request (named by its id, with `classname` set to its file). A failed request gets a `<failure>`, an errored one an `<error>` and a skipped one `<skipped>`; their `message` is the first reason, and the element text lists them all:

```sh
rustman run . --env ci --reporter junit:out/junit.xml --reporter pretty
```

```xml
<testcase name="users/create" classname="users/create.rman.yaml" assertions="1" time="0.008">
    <failure message="status: expected 201, got 404">status: expected 201, got 404</failure>
</testcase>
```

`tap` writes a [TAP version 14](https://testanything.org/tap-version-14-specification.html) stream: one test point per request, `# SKIP <reason>` for skipped requests, and a YAML block with the reasons under each request that failed or errored. An interrupted run ends with `Bail out!`.

```text
TAP version 14
1..2
ok 1 - users/list
not ok 2 - users/create
  ---
  status: failed
  file: "users/create.rman.yaml"
  reasons:
    - "status: expected 201, got 404"
  ...
```

Every reporter writes from the same redacted records: secret values and sensitive headers appear as `«redacted:<name>»` in all of them.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Every executed request passed. |
| 1 | At least one request failed (an assertion, or a required capture) and none errored. |
| 2 | Usage or configuration error: a bad flag, no workspace, an invalid file, a missing environment, an ambiguous request name, no request to run. |
| 3 | At least one request errored: network, TLS, timeout, auth or unresolved variable. It wins over 1. |
| 130 | Interrupted by Ctrl+C (SIGINT). |

`rustman import` has its own: 0 when the import completed (even with unsupported features, unless `--fail-on-unsupported`), 1 with `--fail-on-unsupported` when a feature was unsupported, dropped or unmapped, and 2 for an unreadable, invalid, oversized or undetectable source (see [Importing collections](import.md#exit-codes)).

On Ctrl+C, in-flight requests are cancelled, the requests not finished are reported as `skipped` with `skip_reason: interrupted`, every reporter writes its partial report (with `summary.interrupted: true`), and the exit code is 130. A second Ctrl+C quits at once, also with exit code 130, without waiting for reports to be written.

## Terminal output and logs

Reports and response bodies go to stdout; logs, warnings, errors and Ctrl+C notices go to stderr, so `rustman run . --reporter json -vv > report.json` leaves a valid JSON document in `report.json` and the logs on the terminal.

The `pretty` report is colored only when stdout is a terminal and color is not turned off (`--color never`, a non-empty `NO_COLOR`, or `TERM=dumb`); `--color always` forces it. Piped or redirected output has no ANSI escapes and no carriage-return progress lines. Reports written to a file (`--reporter pretty:out/run.txt`) are never colored.

Logs are off unless `-v` or `RUST_LOG` is given. `RUST_LOG` takes [`tracing` filter directives](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html) and replaces the `-v` levels: `RUST_LOG=rustman_http=debug` shows the requests and headers, `RUST_LOG=trace` everything. Log lines carry only redacted text, and every line is scanned once more for the secret values the run resolved before it reaches stderr.

```text
$ rustman run users -vv --reporter json > report.json
2026-11-17T09:30:00.101Z DEBUG rustman_http::runner::execute: sending request=users/list iteration=1
2026-11-17T09:30:00.113Z  INFO rustman_http::runner::execute: GET http://localhost:8080/users -> 200 (12 ms) request=users/list status=Passed
2026-11-17T09:30:00.113Z DEBUG rustman_http::runner::execute: > Authorization: «redacted:Authorization» request=users/list
2026-11-17T09:30:00.113Z DEBUG rustman_http::runner::execute: < content-type: application/json request=users/list
```

## Completions

`rustman completions <shell>` prints a completion script for `bash`, `zsh`, `fish`, `powershell` or `elvish` on stdout:

```sh
rustman completions bash > ~/.local/share/bash-completion/completions/rustman
rustman completions zsh > "${fpath[1]}/_rustman"
rustman completions fish > ~/.config/fish/completions/rustman.fish
rustman completions powershell >> $PROFILE
```

## OAuth2 without a terminal

When stdin is not a terminal (as in CI), `send` and `run` never open a browser: a request whose `authorization_code` token is not cached fails with `oauth2: interactive login required (run 'rustman auth login <request>')`. See [OAuth2](http.md#oauth2).
