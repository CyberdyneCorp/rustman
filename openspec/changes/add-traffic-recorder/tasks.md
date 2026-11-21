# Tasks

## 1. Crate scaffold and plain-HTTP forward proxy

- [x] 1.1 Create `crates/rustman-proxy` (hyper 1.x, tokio) and the `rustman record` subcommand with `--host`, `--port`, `--allow-remote`. Verify: `cargo build`. An integration test proves loopback bind by default, exit code 2 on a busy port, and exit code 2 for `--host 0.0.0.0` without `--allow-remote`.
- [x] 1.2 Proxy absolute-form HTTP requests upstream through the `rustman-http` connector and record the exchange. Verify: an integration test with a local hyper upstream checks the response passes through byte for byte and a capture exists.
- [x] 1.3 Implement `CONNECT` passthrough (upgrade plus bidirectional copy) with a tunnel entry holding host, port and byte counts. Verify: a test tunnels a TLS echo server and asserts no decryption plus correct byte counts.
- [x] 1.4 Document `rustman record` basics and the printed proxy exports in `docs/record.md`. Verify: the docs build, and a `--help` snapshot test.

## 2. Local CA lifecycle

- [x] 2.1 Implement `ca init` with `rcgen` (ECDSA P-256, CA:true, pathlen:0, 1 year). Store the key in the keychain under the workspace id, and the certificate in the config dir. Verify: unit tests on the certificate fields. A test with a mock keychain asserts no files appear under the workspace, and the no-keychain path fails with exit code 2 and the specified message.
- [x] 2.2 Implement `ca export` (certificate PEM only) and fingerprint printing. Verify: a test parses the output and asserts exactly one CERTIFICATE block and no key.
- [x] 2.3 Implement `ca install`/`uninstall` per platform, with the command printed first, typed `yes` confirmation, and a record of the stores touched. Verify: unit tests with a fake command runner assert the exact commands per OS. Answering `no` leaves stores untouched and exits with 1.
- [x] 2.4 Implement `ca rotate`: undo the recorded installs, delete the keychain entry, and generate and print both fingerprints. Verify: a test with a fake runner and mock keychain.
- [x] 2.5 Document the CA security model, with warnings and manual Firefox steps. Verify: a docs review checklist item in the PR. (docs/record.md "Security model" and "Trusting the CA" cover the model, the warnings and the Firefox steps; PR checklist item: "Docs review: CA security model in docs/record.md".)

## 3. HTTPS interception

- [x] 3.1 Match CONNECT hosts against include globs before the handshake. Mint per-SNI leaf certificates (7 days, LRU of 1000) and terminate TLS with `tokio-rustls`. Verify: an integration test where a client trusting the test CA calls an included host gets a full capture, and a non-included host stays a tunnel.
- [x] 3.2 Detect client handshake aborts and emit `TLS REJECTED` entries with the hint. Verify: a test with a client that doesn't trust the CA.
- [x] 3.3 Support HTTP/2 over intercepted connections via ALPN. Verify: an h2 client test captures the request and response.

## 4. Filters, limits and live output

- [x] 4.1 Implement host, method and content-type filters and the default static-asset drop rule (content type plus extension), with `--keep-static`. Verify: table-driven unit tests, plus an integration test showing filtered requests are still proxied.
- [x] 4.2 Implement body truncation at `max_body` and the capture store bounded by `max_captures`/`max_buffer_bytes`. Verify: a test that a 25 MiB response is delivered in full and stored truncated, and eviction tests.
- [x] 4.3 Implement `--json` redacted JSON-lines output using the `environments` redaction step. Verify: a snapshot test, plus a test that `Authorization`/`Cookie` values never appear in stdout.

## 5. Reverse-proxy mode

- [x] 5.1 Implement `--upstream <url>`: origin-form accept, Host rewrite, TLS verify per `http-requests`, shared capture pipeline. Verify: an integration test `GET http://127.0.0.1:9000/v1/users` reaches the upstream and is captured. A self-signed upstream fails unless verification is disabled through the `http-requests` TLS settings.

## 6. Save captures as request files

- [x] 6.1 Implement path templatization (integer, UUID, ObjectId, ULID) with singularized variable names and `id_N` fallback. Verify: table-driven unit tests, including `/orders/<uuid>/items`.
- [x] 6.2 Implement grouping by method plus templated path, file naming, `--into`, and writing through the `workspace` deterministic writer. Verify: two captures `/users/17` and `/users/42` produce one `get-users-id.rman.yaml`, and re-saving gives no diff.
- [x] 6.3 Implement base URL extraction into `--env` (default `recorded`) with `base_url_N` for extra origins. Verify: a test asserts the environment file contents and the warning.
- [x] 6.4 Implement secret detection (header names, field names, entropy warning) producing `{{<name>}}` placeholders declared under `secrets:`, with `--store-secrets` writing to the secret store. Verify: a test greps the whole workspace for the raw token and finds nothing. With `--store-secrets` the keychain mock holds the value.
- [x] 6.5 Implement the end-of-session `--save` path and the TUI save action entry point. Verify: an integration test, a Ctrl+C on a recorded session writes the files.

## 7. HAR export and TUI pane

- [x] 7.1 Implement HAR 1.2 export (`--har` on exit, plus the TUI action) with redaction and comments for tunnel and truncated entries. Verify: the output validates against the HAR 1.2 JSON schema in a test, and redacted values are asserted.
- [x] 7.2 Register the recorder pane through the `tui` extension point: live list, filter, multi-select, detail view, save/export actions, `CA TRUSTED` status badge. Verify: ratatui `TestBackend` snapshot tests for the list, detail view and badge.
- [x] 7.3 Document saving, HAR export and the TUI pane in `docs/record.md`. Verify: docs build.

## 8. Integration

- [x] 8.1 End-to-end: `curl` through `rustman record --intercept localhost --save --har out.har` against a local TLS test API. Assert the request files, environment, HAR, and that no raw secrets exist anywhere in the workspace. Verify: the CI job passes on Linux, macOS and Windows. (`crates/rustman-cli/tests/record.rs` `curl_through_an_intercepting_recorder_saves_request_files_without_secrets`, run by the default `cargo test --workspace` job on all three OSes with the debug-build stand-in keychain, design D3a; it fails in CI when curl is missing.)

## 9. Review follow-ups

- [x] 9.1 Deny-by-default SSRF guard for network clients (every non-globally-routable range, on the resolved addresses). Verify: unit tests per range in `rustman-http` `address`, and a proxy test refusing private targets.
- [x] 9.2 Shared in-flight capture budget (`max_buffer_bytes`) and at most 100 streams per intercepted HTTP/2 connection. Verify: a test that a body past the budget is stored truncated and proxied in full.
- [x] 9.3 Response-head and idle timeouts (`response_timeout_ms`, `idle_timeout_ms`). Verify: tests with short timeouts and paused time.
- [x] 9.4 HAR files readable by the user only. Verify: a Unix mode test.
- [x] 9.5 A `--save` session without requests writes nothing. Verify: library and CLI tests.
- [x] 9.6 Live output: dropped lines are counted and warned about, and queued lines are flushed on stop. Verify: unit tests with a blocked writer.
- [x] 9.7 `rustman record ca remove`. Verify: a library test with a mock keychain and a fake runner, and a CLI test with the stand-in keychain.
