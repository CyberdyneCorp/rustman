# Design

## Context

`bootstrap-core-engine` defines request files, `_collection.yaml`, environments, secrets with their redaction invariant, and a deterministic emitter. It already owns `curl` import and export. `openapi-import` (in `add-openapi-explorer`) owns OpenAPI. This change adds the other common sources through one pipeline. See proposal.md for the motivation.

## Goals / Non-Goals

**Goals:**
- One conversion pipeline shared by every source format, so secret extraction, naming, conflicts and reporting behave identically.
- No secret value ever reaches a request file or report, enforced in one place.
- Byte-identical output for the same input (testable with snapshots).

**Non-Goals:**
- A JavaScript runtime for scripts.
- Lossless round trip of every source feature. Only Rustman-native features round-trip.

## Decisions

### D1. Two-stage pipeline: source parser → neutral IR → workspace writer
Each format has a parser into an intermediate representation (`ImportedCollection { folders, requests, environments, variables, notes }`). The shared stages then run: variable translation → secret extraction → script-to-assertion mapping → slug and conflict resolution → write. *Alternative rejected:* each parser writes files directly. That would duplicate the secret and naming logic five times, and the secret invariant would be only as strong as the weakest parser.

### D2. Secret extraction is the last transform before the writer, and the writer refuses literals
After extraction, every value flagged as sensitive is a `Secret<String>` (core's redaction type). The workspace writer's API accepts only placeholders for sensitive fields, so a parser bug can't produce a literal secret in a file. A test scans every fixture's output for known secret values. *Alternative rejected:* regex scan of output files after writing. A scan catches only what it knows about, and the file would already be on disk.

### D3. Script handling: comments plus a tiny pattern matcher, no JS engine
The patterns in the spec are matched with a handful of anchored regexes over a whitespace-normalized statement. Anything else stays as a comment. *Alternative rejected:* embedding `boa` or QuickJS to evaluate `pm.*` calls. It adds a sandboxing burden, it's against the declarative-assertions decision in core, and users would assume full Postman script compatibility.

### D4. Hand-written parsers for `.bru` and `.http`
Both formats are small, line-oriented grammars. A hand-written parser with line numbers gives precise report locations. *Alternative rejected:* `pest` grammars, which add a build-time dependency for little gain at this size.

### D5. HAR deduplication key
The key is method + normalized URL (query parameters sorted) + a hash of the body. The first occurrence wins, and later ones count as duplicates in the report.

### D6. Exporters use the same IR in reverse
The workspace is loaded through core, converted to the IR, then serialized as Postman v2.1 or `.http`. Placeholders are exported as-is. The round-trip scenario in the spec becomes a property test over fixtures.

### D7. Crate and dependencies
New `crates/rustman-import` depends on `rustman-core` only (no network). `serde_json` for JSON sources, the core YAML crate for Insomnia v5. The Postman schemas are vendored as test fixtures, not as a runtime dependency.

## Risks / Trade-offs

- [Secret detection misses an unusual field name] → name heuristics plus explicit auth fields plus all `Authorization`/`Cookie` headers. The report lists every variable value that *looks* high-entropy (≥ 24 chars, mixed classes) but wasn't moved, as a "review" warning without printing the value.
- [Postman format quirks across exporter versions] → a fixture corpus covering v2.0, v2.1, nested folders, every body mode and auth type, used as snapshot tests.
- [Users expect scripts to run] → the import summary states up front how many scripts were preserved but not executed.
- [Large HAR files] → streaming JSON parse of `log.entries`, plus a 100 MiB default cap.

## Migration Plan

Additive. New subcommands only, no format changes.

## Open Questions

- Whether to also offer `rustman import postman --from-api` with a Postman API key later. It's out of scope now because it needs network access and a credential.
