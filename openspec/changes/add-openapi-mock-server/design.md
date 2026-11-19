# Design

## Context

`add-openapi-explorer` provides the OpenAPI model, `$ref` resolution, the schema synthesizer (`openapi-import`) and the dependency graph (`api-exploration`). `add-webhook-inspector` set the server conventions: `hyper` 1.x, loopback by default, `--allow-remote`, limits, header hygiene. This change puts a request router on top of those pieces. See proposal.md for the motivation.

## Goals / Non-Goals

**Goals:**
- Reuse the explorer's parser, synthesizer and validator, so mock, import and exploration agree on what the document means.
- Deterministic behavior (seeded synthesis and faults), so mock-based tests are reproducible in CI.
- Hot reload that never takes the server down.

**Non-Goals:**
- Prism feature parity beyond the `Prefer` header and validation.
- A general-purpose programmable mock. Templates are substitution only, with no logic.

## Decisions

### D1. Router: compiled route table, not a generic web framework
Routes are compiled into a segment trie (literal children tried before parameter children), which gives the "more literal segments win" rule directly. *Alternatives rejected:* `axum` or `matchit` routers. Routes come from a document at runtime and must be swapped atomically on reload, and `axum`'s type-level routing fights that. `matchit` rejects some conflicting templates that OpenAPI allows (`/a/{x}` and `/a/{y}/b`).

### D2. Hot reload via `ArcSwap<MockState>`
A `notify` watcher (debounced at 200 ms) rebuilds an immutable `MockState` (document, route trie, overrides, validators). If the build succeeds, the state is swapped atomically with `arc-swap`. In-flight requests keep their `Arc` to the old state. The stateful store lives outside `MockState`, so it survives reloads. *Alternative rejected:* `RwLock<MockState>`. Readers would block during a rebuild and could see a half-updated state.

### D3. Validation with the same JSON Schema engine as assertions
Request bodies and parameters are validated with `jsonschema`, the crate core uses for `schema:` assertions. OpenAPI 3.0 schemas are converted (`nullable`, `exclusiveMinimum` booleans) the same way the explorer does. *Alternative rejected:* a second validator, which would give different verdicts between mock and explorer.

### D4. Deterministic randomness
Synthesis is seeded from an FNV-1a mix of `(seed, operation_id, request path)`, and the synthesizer draws its picks from a SplitMix64 stream on that seed (strings for a `pattern` use proptest's regex generator with a ChaCha stream from the next pick, as the explorer's fuzzer does), so the same request gets the same body regardless of call order. Faults use a per-route stream seeded from the mix of `(seed, route)` and advanced per request, which makes a fixed request sequence reproducible. These small generators avoid a `rand` dependency; proptest is already one of `rustman-explore`'s. *Alternative rejected:* a single global RNG, where concurrent requests would make the output depend on ordering.

### D5. Stateful store keyed by the explorer's resource graph
Resource identity (collection path, item path, id parameter, id field in responses) comes from `api-exploration` dependency discovery. The store is a `Mutex<HashMap<collection, Vec<(id, serde_json::Value)>>>` (insertion order kept by the `Vec`; one lock is enough for a local mock), with a per-resource item cap and a byte budget over all resources. Generated ids follow the id schema (`format: uuid` → UUIDv4 from the seeded RNG, integer → incrementing). *Alternative rejected:* hand-configured resources. Users would have to repeat what the document already says.

### D6. Proxy and record reuse existing pieces
Forwarding uses `rustman-http` with the `webhook-inspector` header-hygiene function. Recording converts each exchange through the `collection-import` HAR pipeline's IR (naming, dedup, secret extraction). That code lives in `rustman-import`, so `rustman-mock` depends on it only behind the `record` path.

### D7. Crates and dependencies
New `crates/rustman-mock` with `hyper` 1.x + `hyper-util`, `arc-swap`, `notify`, `dashmap`, `rand_chacha`. Depends on `rustman-explore` (model, synthesizer, graph), `rustman-core`, `rustman-http` (proxy) and `rustman-import` (record). No new system libraries.

## Risks / Trade-offs

- [The mock is mistaken for a security boundary] → loopback by default, `--allow-remote` with a warning, and credential values unchecked unless configured. The docs say plainly that it's a development tool.
- [Synthesized data doesn't look realistic] → named examples come first, and `format`-aware generators cover email, uri, date-time and uuid. Realism beyond that is a non-goal.
- [The `mocks/` folder shows up as an empty collection in the workspace tree] → it holds only `*.rman-mock.yaml` files, which the workspace loader doesn't parse as requests. The TUI can hide folders with no request files (a `tui` concern, noted for coordination).
- [Reload races with a burst of saves] → 200 ms debounce, and the last successful build wins.

## Migration Plan

Additive. A new subcommand, `[mock]` config keys and the `mocks/` convention. Nothing else changes.

## Open Questions

- Whether the TUI tree should hide request-less folders such as `mocks/` and `webhook/` by default. It doesn't affect this change's specs.
