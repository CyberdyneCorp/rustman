# Design

## Context

This change builds on `bootstrap-core-engine`: the request-file format (`version: 1`), environments and secrets with redaction, the `http-requests` engine, `assertions`, `cli-runner` (and its exit codes 0 = ok, 1 = failures, 2 = invalid input) and `request-history`. That change ships `rustman-report` with run-report types and the JSON/JUnit writers only. This change completes it. See proposal.md for the motivation.

## Goals / Non-Goals

**Goals:**
- An exploration on a medium API (≈100 operations) finishes inside the default 15-minute and 2000-probe budgets.
- Every finding can be reproduced from the report alone (curl or exported request file) without re-running the exploration.
- The safety policy is a single choke point in `rustman-core` that all automated senders (explorer now, agent interface in `add-agent-mcp-server`) must go through. There's no bypass path.

**Non-Goals:**
- Exhaustive fuzzing or symbolic execution. Coverage-guided fuzzing isn't possible black-box.
- Inferring business rules beyond CRUD consistency.
- Supporting malformed documents beyond diagnostics. We don't "repair" specs.

## Decisions

### D1. OpenAPI model: `oas3` for 3.0/3.1, our own normalized IR
Parse with the `oas3` crate (3.1-native, reads 3.0 with minor normalization), then lower it into an internal `ApiModel` IR (operations, parameters, bodies, responses, security) that every later stage consumes. Schemas are kept as raw JSON (`serde_json::Value`) for validation with `jsonschema`, which supports both draft-04-ish 3.0 schemas after a `nullable` → `type: [.., "null"]` rewrite and the 2020-12 dialect used by 3.1.
*Alternatives:* `openapiv3` only covers 3.0. `utoipa` is a generator, not a parser. Using raw `serde_json` throughout would scatter version quirks everywhere. The IR confines them to one lowering pass.

### D2. Swagger 2.0: reject, don't convert
Converting 2.0 → 3.0 correctly (body/formData parameters, `produces`/`consumes`, security definitions) is a project of its own, and silent conversion bugs would turn into false findings. We reject it with a pointer to `swagger2openapi`. We can revisit this if users ask.

### D3. Sync is a three-way merge at field granularity
The import snapshot (normalized document) is the merge base. For each operation we compute generated request fields from base and from remote, and compare them with the local file. A field is updated only when local == generated(base). User-only additions (assertions, captures, extra headers) are never touched. Removed operations go to `_removed/` because deleting user files is irreversible.
*Alternative:* overwrite with a backup copy. Rejected because users would lose their edits on every sync.

### D4. Exploration engine: plan as data, execute as a journaled state machine
The planner emits a deterministic `Vec<Probe>` (pure function of IR, graph, seed and settings), which is why `plan` can be a true dry run and `--seed` reproduces it. The executor walks the probes with a `tokio` semaphore (concurrency) and a `governor` rate limiter. It appends each outcome to `journal.ndjson` with fsync on each line. Resume replays the journal to skip completed probes. Lifecycle chains are single probes made of steps, so they resume atomically from the start of the chain.

### D5. Safety policy lives in `rustman-core`, enforced at the sender
`SafetyPolicy::check(&PreparedRequest) -> Result<(), Blocked>` runs immediately before bytes leave the process, after variable resolution and on every redirect hop (redirects are followed manually for policy-governed senders). It owns method gating, the origin allowlist, the production guard, the rate limit and the budgets. The explorer and the agent interface hold only a `GovernedSender`. They can't reach the raw `http-requests` client. This is compile-time enforcement by API shape, not convention.
*Alternative:* checks in the planner only. Rejected because captured URLs and redirects would bypass them.

### D6. Production guard UX: repeat the host
`--production <host>` must equal the target host. A boolean `--yes-production` flag gets added to shell history and scripts and then reused by accident. Repeating the host is a deliberate act that doesn't transfer to another target. Destructive probes are never allowed against production, with no override, by design.

### D7. Probe generation with `proptest` at runtime
`proptest` strategies built from JSON Schemas (types, formats, bounds, patterns via `regex-syntax`, enums) give boundary-biased values and a `TestRng` seeded from `--seed`. We use proptest's value generation only, not its test runner or shrinking.
*Alternative:* a hand-rolled generator. Rejected because it would re-implement pattern and format generation.

### D8. Finding rules are a static registry
Each rule has a stable id (`auth-not-enforced`, ...), a default severity, a description, a remediation hint and a SARIF descriptor. Detectors return rule ids, never free text. That makes diffing and SARIF stable across versions.

### D9. Reports: one IR, many renderers
`rustman-report` holds the canonical `Report` (serde), a run or an exploration, and it's the only input renderers receive; the exploration data types live there and `rustman-explore` builds them. The JSON Schema is checked into `docs/schemas/report.schema.json`, the file `bootstrap-core-engine` first published for `kind: run`: its `kind: run` part is kept verbatim (so old run reports stay valid), and its `kind: exploration` part is generated from the Rust types with `schemars`. A test fails if it drifts. Renderers: `quick-junit` (already in core), a small typed `serde` model for SARIF 2.1.0, and `minijinja` templates for Markdown and HTML, with CSS/JS inlined at build time via `include_str!`. `serde-sarif` was rejected: it generates its types at build time from the SARIF schema (`schemafy`, `syn`, a second `strum`), a heavy build for the dozen fields Rustman writes; a test validates the output against the official SARIF 2.1.0 schema instead. Redaction happens while the report is built, from exchanges that are already redacted by core.

### D10. Leakage detection: curated regex set
A small, versioned regex set (Java/.NET/Python/Node/Go/Ruby stack traces, common SQL error strings, framework debug-page markers, absolute file paths) runs over response bodies capped at 64 KiB. False positives are acceptable at `medium`, and rules can be disabled via `[explore].disabled_rules`.

### D11. `bad-api` lives in `rustman-test-support`, on hyper
The deliberately broken fixture API is `rustman_test_support::bad_api`, not an axum server under `tests/fixtures/bad-api`: the other in-process test servers are hyper servers in that crate, so it adds no dependency (no axum), and the library tests, the CLI end-to-end test and the docs examples all start the same server. Its OpenAPI document sits next to it (`bad_api.yaml`).

## Risks / Trade-offs

- [Heuristic dependency discovery gets the graph wrong] → `explore plan --graph` shows it; `[explore.producers]` in `rustman.toml` lets users pin producers manually.
- [Generated POSTs create junk data on shared staging] → read-only by default, journaled created resources, cleanup with `--allow-destructive` (its deletes go through the policy and the rate limit but not the budgets, so it still runs after a budget ran out), and a "left behind" list.
- [False positives erode trust] → conservative default severities, de-duplication, `--fail-on high` default, and per-rule disable.
- [Rate limiting is still too aggressive for fragile backends] → `--rate` and `--concurrency` can go down to 1, and we back off on 429.
- [Big documents produce huge plans] → probe budget, per-family caps and `--operations <glob>` filtering.
- [Small but adversarial schemas (wide, nested, `allOf` chains) make validation errors or synthesized values grow exponentially] → only the first schema violation is computed, planning asks only whether a variant is valid, and synthesized values stop at a nesting depth and a member cap.
- [Merge logic bugs during sync silently drop edits] → `--dry-run` summary; removed operations are moved, never deleted; property tests on the merge function.

## Migration Plan

Additive only. New `[safety]` and `[explore]` tables in `rustman.toml` are optional with the defaults from the specs. The report schema stays at `rustman.report/v1`: `bootstrap-core-engine` introduced it with `kind: run`, and this change adds `kind: exploration` and the new optional fields additively, so existing run reports remain valid.

## Open Questions

- Should `explore run` also be exposed as a TUI view in this change, or left to a follow-up on top of `add-terminal-ui`? This doesn't affect these specs either way.
- Defaults for the rule-specific severities may be tuned after dogfooding on a few real APIs.
