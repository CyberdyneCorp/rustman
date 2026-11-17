# Tasks

## 1. Shared data rows

- [ ] 1.1 Add `rustman_core::data_rows` with the CSV reader (RFC 4180, header row, BOM stripped, strings) and the JSON reader (array of objects, scalars converted to strings, nested values as compact JSON). Verify: unit tests for BOM, quoted commas, ragged rows, empty file and object-instead-of-array, each checking the exact error message.
- [ ] 1.2 Switch `load-testing`'s `--data` to `data_rows`. If load testing isn't built yet, add a note to its tasks instead. Verify: the bench data-feeding tests pass against the shared reader.
- [ ] 1.3 Add the data-row layer to the variable resolver, directly below in-run captures in the `environments` chain. Verify: a precedence table test covering all eight layers.

## 2. Data-driven runs

- [ ] 2.1 Implement `--data`, `--no-data`, `--data-label` and `--secret-columns`, plus the `_collection.yaml` keys `data` and `secret_columns` (with the path-escape check). Verify: `assert_cmd` tests for the CSV, JSON, default-data and `--no-data` scenarios.
- [ ] 2.2 Reject `--data` combined with `--iterations`, and a secret label column, with exit 2. Verify: CLI tests check the exit code and that no request reached the stub server.
- [ ] 2.3 Clear non-persisted captures at the start of each iteration. Verify: the iterations-are-independent scenario test.
- [ ] 2.4 Implement `--bail` and `--bail=iteration` across iterations. Verify: tests for both, with 3 rows and a failure in row 2.
- [ ] 2.5 Add `iteration` to exchanges and reporters: pretty grouping, JUnit name suffix, and JSON `iteration` (absent without `--data`). Verify: insta snapshots of each reporter; the JSON output validates against the `rustman.report/v1` schema.
- [ ] 2.6 Auto-mark and redact secret columns. Verify: an end-to-end test greps the report and history for the raw password and finds nothing.

## 3. Auth

- [ ] 3.1 Implement Digest: challenge parsing, MD5/SHA-256 and their `-sess` variants, `qop=auth`, a single retry, and refusal of `auth-int`. Verify: RFC 7616 §3.9.1 test vectors, plus a stub-server test of the challenge flow with two round trips in the timing breakdown.
- [ ] 3.2 Cache Digest nonces per (environment, realm, host), with nonce counts and the stale-nonce retry. Verify: tests for preemptive `nc=00000002` and for `stale=true`.
- [ ] 3.3 Buffer replayable Digest bodies up to 10 MiB, and refuse larger ones. Verify: a 50 MiB file test asserts exit 3 and zero bytes received by the stub.
- [ ] 3.4 Implement SigV4 with the `aws-sigv4` crate, signing as the last step and re-signing each redirect hop. Verify: the AWS SigV4 test-suite cases pass byte for byte; a redirect test checks that each hop's signature differs; default headers appear in `SignedHeaders`.
- [ ] 3.5 Add the SigV4 credential sources (fields, `from: env`, static `from: profile`), with the unsupported-profile error, `unsigned_payload`, and the clock-skew hint. Verify: tests with a temporary `HOME` holding a credentials file, an SSO-only profile fixture, and a skew error from the stub.
- [ ] 3.6 Implement JWT signing with `jsonwebtoken` (HS256/384/512, RS256, ES256), defaulting `iat`/`exp`, with `header`, key sources, placement options and `«redacted:jwt»`. Verify: decode-and-verify tests per algorithm; the algorithm/key mismatch error on load; a history redaction test.
- [ ] 3.7 Document the new auth types in `docs/auth.md`, with one example each. Verify: a doc test loads every example request file.

## 4. Snippets

- [ ] 4.1 Build the snippet engine: `minijinja` templates over `ResolvedRequest`, a quoting filter per language, `--list`, and the unknown-language error. Verify: insta snapshots of all 11 languages for a fixture request (JSON body, query, headers, multipart).
- [ ] 4.2 Apply redaction and `--show-secrets`, export `graphql` as HTTP POST, and refuse other kinds. Verify: tests for the redacted bearer token, GraphQL export, and `kafka` exiting 2.
- [ ] 4.3 Add signature-auth handling: native helpers (curl `--digest` and `--aws-sigv4`, httpie `--auth-type digest`, python-requests `HTTPDigestAuth`), otherwise a comment and a stderr warning. Verify: snapshot tests per auth type × language.
- [ ] 4.4 Add a CI job that runs each snippet against a local echo server for every interpreter on the runner (curl, wget, httpie, python, node, go, pwsh, dotnet, cargo). Verify: the job asserts identical method, path, query, headers and body; missing interpreters are reported as skipped, never as passed.

- [ ] 4.5 Implement `--placeholders` (`{{name}}` in shell tools, constants in code) and the `fetch`/`python`/`rust` aliases. Verify: snapshot tests per language in placeholder mode; a test checks that `rustman docs` output and `export snippet --placeholders` match byte for byte.

## 5. TUI

- [ ] 5.1 Add the "Copy as…" picker on `Alt+C`, the per-language palette entries, MRU ordering, and the unsupported-kind toast. Verify: ratatui `TestBackend` snapshots and key-sequence tests.
- [ ] 5.2 Add the OSC 52 clipboard writer, with capability detection and the read-only pane fallback (no support, or over 100 KB). Verify: unit test of the escape sequence bytes; fallback test with the capability turned off.
- [ ] 5.3 Add the revealed-secrets warning row with double-`Enter` confirmation. Verify: a key-sequence test asserting nothing is written after one `Enter`.

## 6. Script migration

- [ ] 6.1 Extend the pattern table with `to.be.ok`, `responseTime`, `text().include` and header values. Verify: one fixture test per mapping.
- [ ] 6.2 Track `pm.response.json()` aliases, and map the chai field ops (`eql`/`equal`, `exist`, `lengthOf`, `a`, `above`, `below`), including bracket indexes. Verify: fixture tests for nested paths and `[0]` indexes.
- [ ] 6.3 Map the variable setters (environment, collectionVariables, globals, variables) to captures, setting `persist` per scope. Verify: fixture tests for header, cookie and tracked-body captures.
- [ ] 6.4 Add the migration summary counts to the import report and pretty output. Verify: a test on a 40-statement fixture asserts `31/40 (77%)`.
- [ ] 6.5 Implement `--migration-report`: Markdown sections with a suggestion table (`every(...)` → schema, `setNextRequest` → collection ordering, the rest → plugin hint), with redaction. Verify: a golden Markdown test, and a grep for secret values that finds none.
- [ ] 6.6 Run a real-world corpus: import 5 public Postman collections, then record and commit the conversion rate as a fixture baseline. Verify: CI fails if the conversion rate drops below the baseline.
