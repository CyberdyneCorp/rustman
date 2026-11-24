# Proposal

## Why

A review against Postman and Newman found the specs cover nearly everything individual developers and CI pipelines use, with four real gaps that block migration:

1. **Data-driven collection runs.** `newman run -d data.csv` runs a collection once per data row. It's one of the most used CI features. Rustman only specs `--data` for load tests.
2. **Common auth schemes.** Digest, AWS SigV4 and JWT bearer are built into Postman. Rustman only offers SigV4 as an example plugin, and has no Digest or JWT.
3. **Code snippets.** Postman's "Code" button generates Python, JavaScript, Go, HTTPie and more. Rustman only exports curl.
4. **Script migration.** Rustman deliberately has no scripting, so imported `pm.test` scripts don't run. The importer maps only a handful of statements to assertions, so heavily scripted collections lose most of their tests on import.

## What Changes

- **Data-driven runs** (`cli-runner`):
  - `rustman run --data <file.csv|file.json>` runs the selected requests once per row, with row values as variables.
  - A collection can name its default data file in `_collection.yaml`.
  - Each iteration is labeled in every reporter. Secret columns are redacted.
  - The CSV/JSON row parser is shared with `load-testing`'s `--data`, so both read files the same way.
- **Built-in auth** (`http-requests`):
  - `digest`: RFC 7616, MD5 and SHA-256, `qop=auth`.
  - `aws_sigv4`: credentials from fields, OS environment variables or a static `~/.aws/credentials` profile.
  - `jwt`: a signed token generated per request (HS256/384/512, RS256, ES256).
- **Code snippets** (`http-requests`, `tui`):
  - `rustman export snippet --lang <lang> <request>` for 11 targets, with the same redaction as `export curl`.
  - TUI "Copy as…" picker on `Alt+C` and in the command palette, copying through OSC 52.
- **Script migration** (`collection-import`):
  - A wider script-to-assertion mapping: `responseTime`, `pm.response.json()` field checks, body text, header values, `to.be.ok`, and more variable setters.
  - A per-import migration summary showing how many script statements were converted.
  - `rustman import --migration-report <file.md>` lists every statement left unconverted, with a suggested manual equivalent where one exists.

Out of scope:
- Executing JavaScript or any other scripting language. Assertions stay declarative.
- NTLM, Hawk, OAuth 1.0 and Akamai EdgeGrid auth. NTLM is bound to the connection rather than the request and is rare outside legacy Windows; all four remain available as `plugins`.
- AWS SSO, assume-role and credential-process chains. Use `secret-providers` or export temporary credentials to environment variables.
- Generating full SDKs or typed clients. Snippets are single requests.
- Postman "flows" and visual workflow builders.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `environments`: the precedence chain gains a data-row layer below in-run captures, specified in `cli-runner` because only data-driven runs populate it. No existing order changes.
- `cli-runner`: adds data-driven iterations (`--data`, `--secret-columns`, a collection-level `data`) and per-iteration report labels.
- `http-requests`: adds the `digest`, `aws_sigv4` and `jwt` auth types, and snippet export.
- `tui`: adds the "Copy as…" picker and clipboard copy.
- `collection-import`: adds the wider script mapping and the migration report.

All of these are added requirements. No existing requirement changes behavior.

## Impact

- `rustman-core`: a new `data_rows` module (CSV/JSON row reader) shared by `cli-runner` and `load-testing`. The row variable layer is added to the variable resolver.
- `rustman-http`: new `auth::digest`, `auth::sigv4` and `auth::jwt` modules, and snippet generators under `snippet/`, with one template per language.
- `rustman-tui`: the picker dialog and an OSC 52 clipboard writer.
- `rustman-import`: new mapping rules and the migration report writer.
- New dependencies: `csv` (row parsing), `aws-sigv4` (request signing only, without the AWS SDK runtime), `jsonwebtoken` (JWT signing). Digest uses `md-5` and `sha2`. Snippets reuse `minijinja`.
- Reports: each exchange of a data-driven run gains an optional `data_row` object; the integer `iteration` is unchanged. This is an additive change to `rustman.report/v1`.
