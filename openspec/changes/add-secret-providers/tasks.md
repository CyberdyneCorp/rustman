# Tasks

## 1. References, resolver and cache

- [x] 1.1 Parse `{{secret:<provider>/<path>[#field]}}` and `{secret: "…"}` environment variables in `rustman-core`. Verify with unit tests for the "Reference in a header" and "Malformed reference" scenarios.
- [x] 1.2 Add the async `SecretProvider` trait returning `Secret<String>`, with typed errors mapped to messages and to exit codes 2 (config) and 3 (the rest); an unresolved reference is exit code 3 whatever its kind. Verify with unit tests mapping each error kind to its message and code.
- [x] 1.3 Hook resolution into the interpolation boundary only. Verify with a spy-provider test showing that opening the workspace or TUI and saving files makes zero fetches ("Opening the TUI doesn't fetch", "Saving keeps the reference").
- [x] 1.4 Add the in-memory TTL cache with single-flight and `zeroize`, plus a "Refresh secrets" action. Verify with "Run reuses a fetched secret" (one fetch for 50 requests), and with a test that scans the workspace, `.rustman/` and the config dir for the value after a run ("Nothing on disk").
- [x] 1.5 Register resolved values with the `environments` redaction set, using reference or variable labels. Copy-as-curl and snippets show references redacted without fetching them. Verify with the "Echoing server" integration test against history and a JSON report, and a "Copy as curl" CLI test whose recording provider gets no call.
- [x] 1.6 Add per-provider timeouts (default 10 s) and the unresolved-secret error path. Verify the "Slow provider" and "Missing field" scenarios (request not sent, exit code 3).

## 2. Configuration

- [x] 2.1 Load `[secret_providers.*]` from `rustman.toml` and the user config, rejecting credential-looking keys and `cmd` or binary paths in the workspace. Verify with "Token in workspace config" and "Workspace tries to define a command" (exit code 2), plus "Unknown provider in a reference".
- [x] 2.2 Report types that were compiled out with the feature name. Verify with a `--no-default-features` CI build test for "Cloud type in a minimal build".
- [x] 2.3 Document provider configuration for every type in `docs/secrets.md`. Verify that each documented config snippet parses in a doc test.

## 3. Built-in providers

- [x] 3.1 `keychain` and `env` providers. Verify "Process environment in CI", plus a keychain round-trip with `secret set`.
- [ ] 3.2 `sops` provider: SOPS format parsing, MAC check, age decryption, PGP via `gpg`, and workspace path confinement. Verify against SOPS-generated fixtures ("age-encrypted file", "Path escape", "PGP without gpg").
- [x] 3.3 `onepassword` provider through the `op` CLI and Connect REST. Verify with a fake `op` binary on `PATH` and a mock Connect server ("op CLI", "op not signed in").
- [ ] 3.4 `vault` provider: KV v1/v2 detection, versions, namespaces, and token/AppRole/OIDC auth. Verify against a `vault server -dev` container in CI ("KV v2 with version", "Permission denied" with no token in the message).
- [x] 3.5 `cmd` provider from user config, running argv with no shell. Verify the "User-defined command" scenario with a test script.

## 4. Cloud providers (feature-gated)

- [ ] 4.1 `aws-secrets-manager` and `aws-ssm` behind `secrets-aws`. Verify against LocalStack in CI ("AWS JSON secret field", "No cloud credentials").
- [x] 4.2 `gcp-secret-manager` behind `secrets-gcp`. Verify with a mock Secret Manager REST server and fake ADC credentials.
- [x] 4.3 `azure-key-vault` behind `secrets-azure`. Verify with a mock Key Vault REST server and a fake token credential.
- [ ] 4.4 Enable all cloud features in release builds, with a binary-size budget check. Verify with a release CI job that asserts every provider type is listed by `secret list-providers --types`.

## 5. CLI, agents and plugins

- [x] 5.1 Add `rustman secret get` (masked, with `--reveal` only on a TTY), `secret test` and `secret list-providers`. Verify with CLI tests for "Masked by default", "Reveal into a pipe refused" (exit code 2) and "Provider health check" (exit code 3).
- [x] 5.2 Agent interface: request definitions show reference text, results are redacted, and no secret tools are exposed. Tool calls whose arguments contain a reference are refused, and inserted values are never resolved again. Verify with MCP integration tests for "Agent reads a request file", "Agent sends the request" and "Agent writes a reference into an ad-hoc request", a resolver test for "A captured value holds a reference", plus a tool-list assertion.
- [x] 5.3 Plugin boundary: provider values reach plugins only with a `secrets:read` grant. Verify the "Auth plugin without grant" scenario with the plugin host test harness, once `add-plugin-system` exists. Done with `add-plugin-system`: the runner makes every secret a send resolves (provider values included) a secret of the plugin session, and `secrets:read:<provider>/<path>[#field]` grants a direct reference; verified by `auth_plugin_without_grant_sees_the_provider_placeholder` and `auth_plugin_with_the_reference_grant_sees_the_provider_value` (rustman-cli `tests/plugins.rs`) and `learned_provider_values_need_their_reference_grant` (rustman-plugin `tests/secrets.rs`).

## Workflow follow-up

- Archive the change once implementation and review are complete (`openspec archive add-secret-providers`).
