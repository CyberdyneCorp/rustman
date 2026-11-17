# Tasks

## 1. Repository scaffold and CI

- [x] 1.1 Create the Cargo workspace (`Cargo.toml` with `[workspace.dependencies]`, `resolver = "3"`), `rust-toolchain.toml` pinning the MSRV (1.97, also set as `rust-version` and `license = "MIT OR Apache-2.0"` in `[workspace.package]`), and crates `rustman-core`, `rustman-http`, `rustman-report` and `rustman-cli` (binary `rustman`) with the dependency direction from design D1. Verify: `cargo build --workspace` succeeds, and `cargo tree -p rustman-core` shows no hyper, clap or terminal crates.
- [x] 1.2 Add `.github/workflows/ci.yml`: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` on ubuntu-latest, macos-latest and windows-latest; plus a job running `openspec validate --all --strict`. Verify: the workflow passes on the scaffold PR.
- [x] 1.3 Add `cargo deny` (licenses, advisories, bans on `openssl-sys` and `native-tls`) to CI. Verify: the CI job fails if `openssl-sys` is added to any crate.
- [x] 1.4 Add `README.md` (install, quick start) and a `docs/` skeleton. Verify: the README quick-start commands run against the built binary in a CI smoke step.

## 2. Workspace model and loader (`workspace`)

- [x] 2.1 Implement `rustman.toml` parsing with defaults, unknown-key warnings and the version check. Verify: unit tests for minimal settings, unknown key and `version = 2` (exit-code mapping to 2).
- [x] 2.2 Implement workspace discovery (ancestor search, `--workspace`). Verify: tests for the nested-folder and no-workspace scenarios, including the exact error text.
- [x] 2.3 Define the typed request model (`kind` http/graphql/mcp, the HTTP fields, all body types, auth, settings, assertions, captures) behind `rustman_core::format` using `serde-saphyr` with the D4 options and two-pass loading (no `flatten` or tagged/untagged enums), with `<path>:<line>:` errors. Verify: fixture tests for the minimal request, unknown kind, invalid method token, custom method and repeated query; each error asserts the exact line:column; `no`, `on` and `0755` in a JSON body stay strings.
- [x] 2.4 Enforce workspace-relative file paths (body `file`, multipart `file:`, `ca_bundle`, schema files). Verify: the path-escape test rejects `../../etc/passwd` and symlinks that point outside the workspace.
- [x] 2.5 Implement collections, `_collection.yaml` (order, inherited headers, auth and variables) and ordering. Verify: tests for explicit order, inherited auth and the `auth: none` override.
- [x] 2.6 Implement the deterministic YAML emitter. Verify: golden-file round-trip tests show byte-identical load → save for every fixture.
- [x] 2.7 Implement `rustman init [--bare]`. Verify: `assert_cmd` tests for a fresh init, appending to an existing `.gitignore`, and refusing to re-init (exit 2, no files touched).
- [x] 2.8 Document the request-file format in `docs/request-files.md` with one example per body type. Verify: a doc test loads every example in that file.

## 3. Environments, variables and secrets (`environments`)

- [x] 3.1 Implement environment loading, name validation, active-environment selection (`--env` > `RUSTMAN_ENV` > default) and `rustman env list`. Verify: tests for precedence, invalid name warning, and the not-found error that lists available environments.
- [ ] 3.2 Implement the template parser and resolver with the full precedence chain, escaping, non-recursive expansion and multi-name unresolved errors. Verify: unit tests for each precedence scenario, `\{{`, `a="{{b}}"`, and a test showing no socket is opened on unresolved variables.
- [x] 3.3 Implement the dynamic variables (`$uuid`, `$timestamp`, `$timestampMs`, `$isoTimestamp`, `$randomInt [min max]`, `$randomString n`, `$env NAME`). Verify: tests for distinct UUIDs, bounded random (1000 samples) and the missing `$env` error.
- [ ] 3.4 Implement `SecretStore` with the keyring backend, the file fallback (0600 / user-only ACL, one-time warning), the git-ignore guard, `.rustman/workspace-id`, and `rustman secret set|rm|list` (no echo). Verify: file-backend tests on all OSes, the not-ignored refusal test, and an `#[ignore]`d keychain test run manually on macOS and Windows.
- [x] 3.5 Implement `Secret<T>`, segment provenance in `ResolvedRequest`, and the `Redactor` with sensitive-header rules and the echo substring scan. Verify: a canary-secret test asserts the secret never appears in reporter output, history files, logs, curl export or error messages; `--show-secrets` affects the terminal only.
- [x] 3.6 Document environments, secrets and the CI pattern in `docs/environments.md`. Verify: the doc examples are exercised by an integration test.

## 4. HTTP engine (`http-requests`)

- [x] 4.1 Build the in-process test server (`tests/support`) supporting h1, h2, h2c, TLS via `rcgen`, mTLS, redirects, delay, echo, gzip/br/zstd and large bodies. Verify: a self-test of the server fixtures passes.
- [x] 4.2 Implement the timed connector (DNS, TCP, TLS with ALPN) and hyper client for HTTP/1.1, HTTP/2 and h2c prior knowledge. Verify: tests record `http_version`, all timing phases are non-null on fresh HTTPS, `tls` is null on HTTP, and phases are null on a reused connection.
- [x] 4.3 Implement timeouts (total 30 s default, connect 10 s) with phase-specific errors. Verify: the slow-server test gets `timeout after 500 ms (response)` within 600 ms. (The `--timeout` override flag arrives with `rustman send`/`run` in 6.1/6.2.)
- [x] 4.4 Implement the redirect policy (10 hops, method rewriting, cross-host auth stripping, `follow_redirects: false`). Verify: tests for the chain record, cross-host stripping, 307 body preservation and the loop error.
- [x] 4.5 Implement TLS verification (platform verifier, `ca_bundle`, `--insecure` flagging) and mTLS (PEM, PKCS#12). Verify: self-signed rejection, custom CA success, `insecure: true` flag, and mTLS success/failure tests (request and `rustman.toml [http]` certificates). (The `--insecure` flag arrives with `rustman send`/`run` in 6.1/6.2; until then `verify_tls: false` turns verification off.)
- [x] 4.6 Implement proxies (env vars, explicit, `none`, HTTP CONNECT, SOCKS5). Verify: an in-process CONNECT proxy test and a `NO_PROXY` bypass test.
- [x] 4.7 Implement cookie jars per environment (`.rustman/cookies/<env>.json`) and `rustman cookies clear`. Verify: login → `/me` cookie test and the environment isolation test.
- [x] 4.8 Implement basic, bearer and api_key auth (respecting an explicit `Authorization`). Verify: the header value test for basic and the query placement test for api_key.
- [x] 4.9 Implement OAuth2 client_credentials and authorization_code + PKCE (S256, loopback listener), with the token cache in `SecretStore`, refresh, re-acquire on 401, and `rustman auth login`. Verify: a mock token server test shows one token call for two requests; a PKCE test checks `code_challenge_method=S256`; the non-interactive error test. (`rustman auth login` is always interactive; mapping a non-TTY stdin to the non-interactive error arrives with `rustman send`/`run` in 6.1/6.2.)
- [x] 4.10 Implement body handling: default headers, computed `Content-Length`/`Host` with a warning on stale values, decompression, the 10 MiB in-memory cap with spill to `.rustman/bodies/`, and binary detection. Verify: a 50 MiB spill test with a memory ceiling (via a counting allocator in tests), the PNG byte-exact test and the stale Content-Length test.
- [x] 4.11 Implement `rustman import curl` and `rustman export curl` with redaction. Verify: import tests for the listed flags, the unsupported-flag warning, the redacted export test, and a proptest for the export → import round trip.
- [x] 4.12 Document the HTTP settings, auth and curl interop in `docs/http.md`. Verify: the doc snippets are covered by tests from this group.

## 5. Assertions and captures (`assertions`)

- [x] 5.1 Implement the assertion engine (evaluate everything; pass/fail/error) with status, header, body and latency assertions. Verify: unit tests per scenario, with exact failure message snapshots.
- [x] 5.2 Implement JSONPath assertions (`serde_json_path`) with all the ops, and the non-JSON error. Verify: tests for equals, length, missing path (fail, not error) and multi-node comparison.
- [x] 5.3 Implement JSON Schema assertions (file and inline, 2020-12 and draft-07, max 10 violations). Verify: the `/email` violation message test.
- [x] 5.4 Implement interpolation in expected values, and captures (json, header, status, body_regex, cookie; `required`; `persist` to `.rustman/state/<env>.yaml`; secret captures to `SecretStore`). Verify: the token-chaining collection test, the required-capture failure, persisted-capture-across-runs, and a check that `environments/` stays unmodified.
- [x] 5.5 Document assertions and captures in `docs/assertions.md`. Verify: every documented example is a passing fixture.

## 6. CLI runner (`cli-runner`)

- [x] 6.1 Implement `rustman send` (by path or name, `--body-only`, `-o`, `--timeout`, `--insecure`, the ambiguous-name error) and ad-hoc send outside a workspace. Verify: `assert_cmd` tests, including `--body-only` byte equality and running without a workspace.
- [x] 6.2 Implement `rustman run` with `--env`, `--var`, `--timeout`, `--insecure`, `--bail`, `--filter`, `--iterations`, the folder-sequential/parallel scheduler and ordered results. Verify: bail and filter tests, and a parallel test using server-side timestamps to show folders overlap while in-folder order holds.
- [x] 6.3 Implement the reporters in `rustman-report` (pretty, json `rustman.report/v1 (kind: run)` with a published JSON Schema at `docs/schemas/report.schema.json`, junit via `quick-junit`, tap), multi-reporter output and stdout default. Verify: insta snapshots per reporter; the JSON report validates against its published schema; JUnit is checked with an XML parser.
- [x] 6.4 Implement the exit-code mapping (0/1/2/3/130) and SIGINT cancellation with partial report flush. Verify: tests per exit code, including "3 wins over 1"; a SIGINT test on Unix (spawn, signal, assert 130 and a valid partial JSON report).
- [x] 6.5 Implement terminal behavior (TTY detection, `NO_COLOR`, `TERM=dumb`, `--color`), stderr/stdout separation, and `-v`/`RUST_LOG` with redacting logs. Verify: tests show no ANSI escapes with `NO_COLOR=1` or when piped, and that stdout is valid JSON with `-vv`.
- [ ] 6.6 Implement `--help`, `--version` and `rustman completions <shell>`. Verify: snapshot tests of help output; generated completion scripts parse in bash/zsh/fish on CI (ubuntu).
- [x] 6.7 Document the CLI reference and exit codes in `docs/cli.md`. Verify: a test checks that every subcommand appears in `docs/cli.md`.

## 7. Request history (`request-history`)

- [x] 7.1 Implement recording (ULID files, workspace or platform data dir, `--no-history`, `[history] enabled`) through the `Redactor`. Verify: tests for send-is-recorded, disabled and the canary secret never on disk.
- [x] 7.2 Implement retention (500 default, spill-file cleanup, `history clear`) with atomic writes and a pruning lock. Verify: the 501st-write prune test, and a concurrent-writers test (two processes × 50 requests) where every entry is readable.
- [x] 7.3 Implement `history list` filters (`--status`, `--method`, `--grep`, `--since`, `--limit`, `--json`), `show` and `replay` with secret re-resolution. Verify: filter tests; replay with the secret present (real value sent) and with it deleted (unresolved error, nothing sent).
- [x] 7.4 Document history in `docs/history.md`. Verify: the doc examples run in an integration test.

## 8. Integration checks

- [ ] 8.1 End-to-end smoke test: `rustman init` → add an environment and secret → run the sample collection against the in-process server with the json and junit reporters → replay from history. Verify: it passes on all three CI operating systems with no network access (CI job with networking disabled on Linux).
- [x] 8.2 Run the full redaction audit: grep every artifact produced by the e2e test for the canary secret, including a non-ASCII secret with a `"` echoed `\u`-escaped, an assertion message cut mid-secret and a spilled body whose preview cut falls mid-secret (spilled bodies themselves are documented working state). Verify: zero matches of any piece of either secret.

## Workflow follow-up

- Archive with `openspec archive bootstrap-core-engine` once every task above is checked. This creates `openspec/specs/{workspace,environments,http-requests,assertions,cli-runner,request-history}/spec.md`.
