# Contract fixtures

Inputs of the `rustman-contract` tests (`crates/rustman-contract/tests/`).

- `rules/<rule-id>.yaml`: one fixture pair per rule of the catalog. Each file holds an `old` and a `new` document (completed with `openapi: 3.1.0`, `info` and an empty `paths` when missing) and the rule ids the diff must report, in order (`expect`).
- `model/*.yaml`: the same format, for the normalized model: renamed path parameters, merged path-item parameters, OpenAPI 3.0 `nullable`, recursive schemas, `$ref` refactors and the "Two versions compared" scenario.
- `corpus/<name>/{old,new}.yaml`: version pairs loaded from disk the way `rustman contract diff` loads them, with every expected difference in `expected.txt`. Regenerate an expectation deliberately with `RUSTMAN_UPDATE_CORPUS=1 cargo test -p rustman-contract corpus` and review the diff.

The corpus documents are hand-trimmed excerpts in the shape of public API descriptions, not verbatim copies (the full documents are several megabytes each):

- `petstore`: `fixtures/petstore.yaml` (OpenAPI 3.0, `allOf`, shared parameters, security schemes) and a later version.
- `stripe`: the customers endpoints in the style of Stripe's published OpenAPI document (form-encoded bodies, `anyOf` expansions, `nullable`, two security alternatives).
- `github`: repository and issue endpoints in the style of GitHub's REST API description (component parameters and responses, `oneOf` labels, preview endpoints that were removed).
