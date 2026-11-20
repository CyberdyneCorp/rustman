# Pact JSON schemas

`pact-schema-v3.json` and `pact-schema-v4.json` are the JSON Schemas of Pact
files published in [pactflow/pact-schemas](https://github.com/pactflow/pact-schemas)
(`dist/`, commit `900e42c6b52c8315a66701939fbea43e0617ffe0`, ISC license),
copied with their content unchanged (re-indented). `crates/rustman-contract/tests/pact_generate.rs` validates
the Pact files `rustman contract pact generate` writes against them.

The published schemas have no `status` category under `matchingRules`, which
the Pact reference implementation reads for `statusCode` matchers (written as
`"status": {"$": {...}}`); the test adds that category before validating.
