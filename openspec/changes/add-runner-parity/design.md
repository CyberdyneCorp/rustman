# Design

## Context

This change involves four existing capabilities:
- `cli-runner` owns `rustman run`, which already has `--iterations <n>`.
- `http-requests` owns auth helpers (basic, bearer, API key, OAuth2) and `export curl`.
- `load-testing` already specs a `--data` option for benches.
- `collection-import` preserves Postman scripts as comments, and maps four statement shapes to assertions.

The goal is parity with Postman and Newman on the features users actually hit when migrating, without adding a scripting language. See proposal.md for the motivation.

## Goals / Non-Goals

**Goals:**
- `rustman run --data` behaves like `newman run -d`, so existing CI pipelines can switch with a flag rename.
- The new auth types produce byte-identical signatures to the reference implementations: RFC 7616 test vectors and the AWS SigV4 test suite.
- Snippets are runnable as printed, and CI proves it for every language whose interpreter is available.
- Secrets never leak through any new path: data rows, snippets, the clipboard, or migration reports.

**Non-Goals:**
- Scripting, NTLM, OAuth 1.0, or AWS credential chains beyond static keys (see proposal.md).
- Matching Postman's iteration semantics exactly where they're surprising. For example, Newman repeats the last row when `-n` exceeds the row count; Rustman rejects that combination instead.

## Decisions

### D1. One shared row reader in `rustman-core`
CSV and JSON parsing lives in `rustman_core::data_rows`, which `cli-runner` and `load-testing` both call. This guarantees `--data users.csv` means the same thing in `run` and `bench`.
- CSV follows RFC 4180. The first row is the header, a UTF-8 BOM is stripped, and every value is a string.
- JSON must be an array of objects. Scalar values are converted to strings; nested values become compact JSON text.
- *Rejected:* each command parsing on its own (the two would drift), and `polars` (far too heavy for row iteration).

### D2. Data rows sit directly below in-run captures
The `environments` precedence chain gains one layer. Highest first: runtime override > in-run captures > **data row** > request `variables` > collection `variables` > persisted captures > environment > `rustman.toml`.
- A row value beats request and collection `variables`, so those act as defaults that the data file varies.
- A row value beats the environment, so a row can change `{{base_url}}` per tenant.
- A capture made during the iteration beats the row, so a login capture can replace a row's placeholder token. This matches Postman, where data variables rank above environment variables.
- In-run captures are cleared at the start of each iteration, so iterations stay independent and their results can be compared. Values written with `persist: true` live in the persisted-captures layer and stay available.
- *Rejected:* row above captures, which would make `login → token` chains impossible to override within an iteration.

### D3. `--data` and `--iterations` are mutually exclusive
With `--data`, the iteration count is the row count. Combining the two is a usage error (exit 2). Newman's repeat-the-last-row behavior when `-n` is larger than the row count surprises people and hides short data files.

### D4. `aws-sigv4` crate for signing, not the AWS SDK
The `aws-sigv4` crate is the signing core of the AWS Rust SDK, without the runtime, the credential chain or a hard dependency on any HTTP client. Signing is the last step before send, after interpolation, default headers and collection inheritance. Each redirect hop on the request's origin is re-signed for its own URL; once a redirect leaves the origin, no further hop is signed (matching how `Authorization` is dropped for every other auth), so no other host ever receives a signature, access key or session token made with the request's credentials. Credentials come from three sources:
- explicit fields, usually `{{secret}}` variables;
- `from: env`, reading `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and `AWS_SESSION_TOKEN`;
- `from: profile`, reading static keys from `~/.aws/credentials`.

*Rejected:*
- `aws-config` with the full SDK: it adds megabytes and a tokio-bound credential chain, and SSO/assume-role are out of scope.
- Hand-rolled SigV4: canonicalization edge cases are a known source of bugs.

### D5. Digest is implemented in-house, retrying once after a 401
The flow is:
1. The first request is sent without credentials.
2. On a `401` with `WWW-Authenticate: Digest`, the system computes the response and resends once.
3. The nonce and nonce count are cached per (environment, realm, host), so later requests send credentials up front.
4. A stale nonce (`stale=true`) triggers one fresh retry. A second `401` is a normal response, not a loop.

Supported: MD5, MD5-sess, SHA-256, SHA-256-sess, `qop=auth`. Not supported: `auth-int` and `userhash`. A request with a streamed `file` body is buffered up to 10 MiB so it can be replayed; anything larger fails with a clear error.
- *Rejected:* `diqwest` and `digest_auth` crates. The first is tied to reqwest; the second has no SHA-256-sess support and no maintenance in two years.

### D6. JWT tokens are generated per request with `jsonwebtoken`
`auth: {type: jwt}` signs a token from `claims` (after interpolation), adds `iat`, and adds `exp = iat + ttl` (default `ttl: 5m`). The token goes in `Authorization: Bearer` by default, or in a named header or query parameter. Keys are `{{secret}}` variables or files inside the workspace. The token is always redacted in history and reports as `«redacted:jwt»`.
- *Rejected:* caching signed tokens. Generating one per request is cheap and avoids clock-skew bugs.

### D7. Snippets come from `minijinja` templates over one resolved request model
Every target renders from the same `ResolvedRequest` that `export curl` uses: method, URL, headers, body parts, and auth after resolution and redaction. Each language is one template file plus a small quoting filter, so adding a language means adding a template.

Signature auth (`digest`, `aws_sigv4`, `jwt`) can't be precomputed into a static header, because it depends on time or a server challenge. For these, the snippet uses the language's native helper where one exists (for example `requests.auth.HTTPDigestAuth`, or `--auth-type digest` for HTTPie). Otherwise it adds a comment naming the auth type, and the CLI prints a warning.
- *Rejected:* `httpsnippet` via a bundled JS engine (adds an interpreter to the binary), and hand-written string builders (hard to maintain).

### D8. OSC 52 for the clipboard
The TUI copies through the OSC 52 escape sequence. It works over SSH, inside tmux (with `set-clipboard on`) and in most modern terminals, and needs no system clipboard library or display server.
- When the terminal doesn't advertise support, or the payload is over 100 KB, the snippet opens in a read-only pane for manual copy instead.
- *Rejected:* `arboard`. It needs X11/Wayland libraries on Linux, which conflicts with the no-system-libraries constraint, and it doesn't work over SSH.

### D9. Script mapping uses a token pattern matcher, not a JavaScript parser
The importer recognizes a fixed set of statement shapes with a small tokenizer and pattern table. Anything outside the table stays comment-only and goes into the migration report.
- *Rejected:* `swc` or `boa` to parse real JavaScript. Both add a large dependency for a one-time import, and a real parser invites "almost supports scripts" expectations.

### D10. One snippet engine for the CLI, the TUI and generated docs
`add-docs-generator` specs its own code-sample templates over a `SampleRequest`. To stop the two from drifting, the snippet engine in `rustman-http` owns all templates and adds a placeholder mode that keeps variables unresolved. `api-docs-generation` calls the engine in that mode and keeps its `[docs].template_dirs` override on top of it. The docs language ids (`fetch`, `python`, `rust`) are aliases of the snippet ids.

## Risks / Trade-offs

- **Postman users expect scripts.** The migration report makes the gap visible and actionable instead of silent. Plugins cover custom logic.
- **SigV4 clock skew.** A clock off by more than 5 minutes makes AWS reject the request. The error message points at the `Date` header and suggests checking the system clock.
- **Request bodies are buffered.** The engine sends every body as one in-memory buffer (a v0.1 design), so `unsigned_payload: true` skips hashing a large body but does not stream it. Streaming file bodies is an engine-wide change, left to a follow-up. Until then the unsigned-payload scenario's streaming clause is not met, and task 3.5 stays open pending the user's decision.
- **`rustman.report/v1` stays compatible.** v0.1 writes a required integer `iteration` on every request; it keeps that type and meaning. The data row goes in a new optional `data_row` object, present only with `--data`, so the change is additive and a run without `--data` writes the v0.1 report unchanged.
- **OSC 52 support varies by terminal.** The read-only pane fallback always works.
