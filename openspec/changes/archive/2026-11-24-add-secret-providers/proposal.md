# Proposal

## Why

`environments` stores secrets in the OS keychain, which works for one person on one machine. Teams already keep credentials in 1Password, HashiCorp Vault, cloud secret managers or SOPS-encrypted files. Today they'd have to copy those values into every developer's keychain and every CI runner by hand. That copying drifts out of date, leaks into shell history, and makes rotating a credential painful. Rustman should read secrets where they already live, at send time, without ever writing them anywhere.

## What Changes

- Add **secret references**, `{{secret:<provider>/<path>[#field]}}`, resolved at send time and never persisted. Environment variables can also be backed by a reference.
- Add configurable **secret providers** in `[secret_providers.<name>]` in `rustman.toml`. Config only, never credentials. Credentials come from each provider's native auth chain.
- Built-in provider types:
  - `keychain` (the existing store), `env` and `sops` (age natively; PGP through an installed `gpg`)
  - `onepassword` (the `op` CLI or a Connect server)
  - `vault` (KV v1/v2; token, AppRole and OIDC auth)
- Cloud provider types, behind cargo features enabled in release builds: `aws-secrets-manager`, `aws-ssm`, `gcp-secret-manager` and `azure-key-vault`.
- `cmd`: runs a command to produce a secret. It's off by default and can only be defined in the user's config, never in a workspace.
- In-memory cache with a TTL (default 5 minutes). Never written to disk.
- Every resolved value joins the redaction set from `environments`, so it's masked in the TUI, history, reports and agent results.
- Failure handling:
  - an unresolved secret is a request error (exit code 3), naming the provider and path but never the value
  - a misconfigured provider is a configuration error (exit code 2) when the configuration loads; a problem found only while resolving (a type not in this build, a missing region or address) leaves the reference unresolved (exit code 3)
- CLI: `rustman secret get|test|list-providers`. `get` masks the value unless `--reveal` is passed on a TTY.
- Agents never receive secret values.

Out of scope: writing or rotating secrets in external providers; a Rustman-hosted secret store; sharing secrets between team members (that's the provider's job); provider plugins (plugins may add providers later through `plugins`).

## Capabilities

### New Capabilities
- `secret-providers`: secret reference syntax and resolution, provider configuration and types, credential sourcing, caching, failure modes and exit codes, redaction integration, the `secret` CLI commands, and agent and plugin boundaries.

### Modified Capabilities
None. `environments` keeps its keychain secrets and its redaction invariant unchanged. Provider-resolved values are added to that redaction set through its existing contract.

## Impact

- New module `secrets/` in `rustman-core` (reference parsing, resolver, cache, provider trait). REST-based providers sit on `rustman-http`.
- Optional cargo features `secrets-aws`, `secrets-gcp` and `secrets-azure` add the cloud providers (REST clients on `rustman-http` with each platform's credential chain). They're off for plain `cargo build` and on for official release binaries.
- New dependencies: `age` (SOPS age decryption). PGP decryption shells out to `gpg` only when it's installed.
- `rustman.toml` gains `[secret_providers.*]`. The user config (`~/.config/rustman/config.toml`) gains `[secret_providers.*]` for `cmd` and for overriding binary paths.
- `rustman-cli` gains `secret get|test|list-providers`. `secret set` from `environments` stays as it is.
