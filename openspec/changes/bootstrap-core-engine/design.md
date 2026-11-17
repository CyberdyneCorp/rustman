# Design

## Context

The repository is empty. `openspec/config.yaml` fixes the constraints: stable Rust (edition 2024), first-class Linux/macOS/Windows, a default binary with no native system libraries, local-first, the secret redaction invariant, and an engine with no UI code. `openspec/ROADMAP.md` fixes the crate layout and vocabulary. This change builds the four crates everything else depends on. See proposal.md for scope.

## Goals / Non-Goals

**Goals:**
- One engine API that the CLI uses now, and that the TUI, the MCP server and the explorer reuse unchanged later.
- Exact, per-phase timings (DNS / connect / TLS / TTFB / download), because later changes chart them in the TUI and in the viz companion.
- A request-file format that's stable enough to freeze at `version: 1`.
- Redaction enforced by the type system rather than by convention.

**Non-Goals:**
- Preserving YAML comments when Rustman rewrites a file (see Risks).
- A plugin system or scripting runtime.
- An HTTP/3 transport.

## Decisions

### D1. Crates created in this change
`rustman-core` (model, workspace loader, variables, secrets, safety-policy types, the redaction type), `rustman-http` (transport, auth, timings, curl), `rustman-report` (`rustman.report/v1 (kind: run)` types plus the json, junit and tap writers) and `rustman-cli` (the `rustman` binary). Later crates from ROADMAP are added by their own changes. The dependency direction is strict: `cli → report, http → core`. `core` has no network or terminal dependencies.

### D2. Transport on hyper 1.x with a custom connector (reqwest rejected)
reqwest can't report DNS, TCP connect and TLS handshake as separate phases. Its `connector_layer` wraps connect and TLS together, and its resolver hook can time DNS but nothing after it. The spec needs all of these phases. `rustman-http` therefore uses `hyper` + `hyper-util` (client, HTTP/1.1 and HTTP/2) with its own `tower::Service` connector. That connector does a timed DNS lookup (`hickory-resolver`, falling back to the system resolver via `tokio::net::lookup_host`), a timed `TcpStream::connect`, an optional proxy tunnel (HTTP CONNECT is hand-written over the stream; SOCKS5 uses `tokio-socks`) and a timed `tokio-rustls` handshake with ALPN `h2,http/1.1`. TTFB and download are measured around the response future and the body stream.
- The cost is that we implement redirects, cookies (`cookie_store`, which follows RFC 6265), decompression (`async-compression`) and proxy-env parsing ourselves. These features are in the spec anyway and benefit from being explicit.
- Alternatives: `isahc`/libcurl, which has real phase timings but links libcurl (a C library), violating the constraint. `reqwest` plus approximate timings, which fails the spec.

### D3. TLS: rustls with the `ring` provider and `rustls-platform-verifier`
The platform verifier uses the OS trust store (Keychain, SChannel, or the Linux bundle) and supports revocation where the OS does. When `ca_bundle` is set, a `WebPkiServerVerifier` built from the bundle plus the platform roots is used. `--insecure` swaps in a verifier that accepts any certificate but still records `insecure: true` on the exchange. mTLS uses `rustls-pemfile`. PKCS#12 bundles go through the `p12-keystore` crate (pure Rust).
- `ring` over `aws-lc-rs`: aws-lc-rs needs CMake and a C toolchain on some targets. ring needs only the C compiler that Rust targets already assume (MSVC on Windows), and no system libraries. OpenSSL (`native-tls`) is rejected because it's a system library.

### D4. YAML: `serde-saphyr` (serde_yaml rejected)
`serde_yaml` is deprecated and archived. `serde_yml` (a fork) had soundness and maintenance concerns. We use `serde-saphyr` (pinned `=2.1.x`, MSRV 1.89): pure Rust, with serde support and no `unsafe` libyaml port. Since 2.x it parses with `granit-parser` (a `saphyr` descendant). Line and column positions from parse errors feed the `<path>:<line>:` messages.
- Spike result (2026-11-17, serde-saphyr 2.1.1), which this design now depends on:
  - Load → save → load is value-equal, and output is byte-stable after the first save.
  - Errors carry exact line:column for unknown fields, bad indentation, unclosed quotes, duplicate keys (rejected by default) and wrong types, **except** inside `#[serde(flatten)]`, internally tagged and untagged enums. There serde buffers the content and positions drift to the end of the mapping. `deny_unknown_fields` also doesn't work with `flatten` (a serde limitation).
  - The default budget rejects an alias bomb (250k nodes) in ~60 ms. 2000 request files parse in ~35 ms; a 5 MiB string body in ~3 ms.
  - Defaults follow YAML 1.1 scalar rules for untyped values (`no` → false, `0755` → 755.0), which would corrupt `body.json` and variables.
- Rules that follow from the spike:
  - Always parse with `strict_booleans: true` and `leading_zero_numbers_as_strings: true` (only `true`/`false` are booleans; `0755` stays a string). Keep the default budget and `DuplicateKeyPolicy::Error`.
  - The request model uses no `flatten`, internally tagged or untagged enums. Loading is two-pass: read `version` and `kind` into a small header struct, then deserialize the kind-specific struct (`deny_unknown_fields`) from the same source. Assertions use an explicit key per type and a hand-written `Deserialize`. This keeps every error position exact.
- Fallback if `serde-saphyr` blocks us: `serde_norway`, a fork with the serde_yaml API (last release 2024-12, so treat it as a stopgap). Hiding the crate behind the `rustman_core::format` module makes swapping a one-module change.
- For byte-stable output, writes don't use the generic serializer directly. A small emitter in `rustman_core::format::write` walks the typed model in schema order with fixed quoting rules. That gives the "Stable serialization" requirement a single owner and a golden-file test suite.

### D5. Settings and data formats
`rustman.toml` uses `toml` + `serde`. History, cookies, state and reports use JSON (`serde_json`). One exchange is stored per file as `.rustman/history/<ulid>.json`. ULIDs sort by time, so listing is just a sorted directory scan. Spill bodies go in `.rustman/bodies/<ulid>.bin`. Pruning and writes use temp file + `rename`, with an advisory lock (`fs4`) on `.rustman/history/.lock` around pruning.
- SQLite (`rusqlite`) is rejected: its bundled build compiles C, and 500 small files is well within filesystem comfort. If search becomes slow, a later change can add a derived index file without changing the spec.

### D6. Secrets: the `keyring` crate (v3) behind a `SecretStore` trait
The keychain service is `rustman`, the account `<workspace-id>/<env>/<name>`. `workspace-id` is a UUID generated once into `.rustman/workspace-id` (git-ignored), so two clones on one machine don't collide. A per-user id isn't committed, because a committed id would make two clones share secrets silently. The fallback `FileSecretStore` writes `.rustman/secrets.yaml` with mode 0600 on Unix (on Windows, the ACL is restricted to the current user). It first checks `git check-ignore -q .rustman/secrets.yaml`. Without git or a repo, the check passes.
- OAuth2 tokens use the same store under the account `<workspace-id>/<env>/oauth2:<request-or-collection-id>`.

### D7. Redaction is a type, not a filter
Secret values are carried as `Secret<String>`, a newtype with no `Display`, a `Debug` that prints `«redacted:name»`, and an explicit `.expose()` that only the transport writer calls. Resolution returns a `ResolvedRequest` whose header and body segments record which byte ranges came from secrets. Every output sink (reporters, history, logs, curl export, error messages) serializes through a `Redactor` that substitutes those ranges and any sensitive header. A second safety net does a substring scan of the outgoing artifact for known secret values. That catches servers that echo the secret back, as in the spec scenario. Tests assert the invariant on every sink with a canary secret.

### D8. Variable resolution
The template is parsed once into segments (`Literal`, `Var(name)`, `Dynamic(fn, args)`). Resolution walks the precedence chain from the environments spec as a `Vec<&dyn VarSource>`. Every missing name is collected before failing, so the error lists all of them. Dynamic variables use `uuid` (v4), `rand` and `time`.

### D9. Assertions and captures
JSONPath uses `serde_json_path` (RFC 9535 compliant). JSON Schema uses the `jsonschema` crate, with drafts 2020-12 and 7. Regex uses the `regex` crate, which has linear-time matching and no catastrophic backtracking. Assertions are a serde-tagged enum, evaluated into `AssertionResult { kind, outcome, expected, actual, message }`. Captures run after assertions and write into the run-scoped `VarScope`.

### D10. Async runtime and concurrency
`tokio` (multi-thread). The runner builds a DAG in which folders are units of sequential work. `--parallel n` is a `tokio::sync::Semaphore` over folder tasks. Results are collected into slots indexed by collection order, so reporters always print in order. SIGINT uses `tokio::signal::ctrl_c` → a `CancellationToken` (`tokio-util`) that's checked by the transport and the runner.

### D11. CLI
`clap` (derive) + `clap_complete`. Color goes through `anstream`/`anstyle`, which honor `NO_COLOR`, `TERM=dumb` and `--color`. Progress uses `indicatif`, only when stderr is a TTY. Logging uses `tracing` + `tracing-subscriber` (`RUST_LOG`, `-v`), with a redacting field formatter.

### D12. Error taxonomy and exit codes
Each library crate uses `thiserror` enums. The CLI maps them onto exit codes:

| Error class | Examples | Exit |
|-------------|----------|------|
| `ConfigError` | no workspace, invalid file, bad flag, unknown env | 2 |
| `RequestError::{Network, Tls, Timeout, Unresolved, Io, Proxy, Auth}` | refused, cert failure, timeout | 3 |
| `AssertionFailure` | any failed assertion | 1 |
| interrupted | SIGINT | 130 |

`miette` renders `ConfigError` with source spans (file:line) for request-file problems.

### D13. Curl interop
Import uses `shell-words` for tokenizing, plus a hand-written flag parser that covers the listed flags. Export builds the argument list from `ResolvedRequest` through the `Redactor` and quotes with `shell-escape`. A property test (`proptest`) checks that export → import gives the same request.

### D14. Testing strategy
- Unit tests in every crate. Golden-file tests for the YAML emitter and the reporters.
- Integration tests against an in-process `hyper` test server (`tests/support/server.rs`) that serves h1, h2, TLS (certs generated with `rcgen`), mTLS, redirects, delays, large and compressed bodies, and echo. The suite never touches the internet.
- A proxy test using an in-process CONNECT proxy.
- A keychain test behind `#[ignore]` on CI. The file fallback is tested everywhere.
- CLI tests with `assert_cmd` + `insta` snapshots for help, reporters and exit codes.

## Risks / Trade-offs

- [Rewriting a hand-edited request file drops its YAML comments] → Rustman only rewrites a file on explicit save or import. Before overwriting a file that contains comments, the save path warns, and offers a `.bak` in the TUI. Comment-preserving editing via the `saphyr` event API is a later enhancement.
- [The hyper-based transport means more code to own (redirects, proxies, cookies)] → each piece is small and specified, and the in-process test server covers it. If proxy edge cases pile up, reqwest can still be used for OAuth token calls only.
- [Keychain prompts or unavailability on headless CI] → file fallback with a warning. The README documents the CI pattern: a CI-only environment whose variables use `{{$env NAME}}`, fed from CI secret variables.
- [ring needs a C compiler on some platforms] → this is documented. The constraint in `config.yaml` is interpreted as "no system libraries or extra native SDKs". See Open Questions.
- [`serde-saphyr` is young] → isolated behind `rustman_core::format`, with `serde_norway` as the drop-in fallback.

## Migration Plan

Greenfield, so there's nothing to migrate. The request-file `version: 1` and `rustman.report/v1 (kind: run)` schemas are frozen at release. Future breaking changes bump the version, and Rustman keeps reading older versions.

## Resolved Questions

- `ring`'s build-time C compiler is accepted under the "no system libraries" rule (recorded in `openspec/config.yaml`). `rustls-rustcrypto` stays experimental; revisit if it stabilizes.
- License: MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`).
- MSRV: 1.97, the highest `rust-version` among planned dependencies (`wasmtime` for `add-plugin-system`). Others need ≤ 1.89.
- YAML crate: `serde-saphyr`, confirmed by the spike in D4.

## Open Questions

None.
