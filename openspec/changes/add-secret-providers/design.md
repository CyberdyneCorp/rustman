# Design

## Context

`environments` (in `bootstrap-core-engine`) owns keychain secrets, the `«redacted:…»` redaction step and the rule that unresolved variables are errors. This change adds a second source of secret values behind the same interpolation and redaction path. The default binary must build with plain `cargo build` and no system libraries. See proposal.md for the motivation.

## Goals / Non-Goals

**Goals:**
- One resolver interface. Adding a provider never touches interpolation, redaction or the TUI.
- Nothing resolved ever reaches disk or an agent.
- A small default binary. Heavy cloud dependencies are optional at compile time but included in release builds.

**Non-Goals:**
- Writing, rotating or leasing secrets (Vault dynamic credentials are read once like any KV value).
- An offline encrypted cache.
- Provider plugins in this change. The trait is designed so `plugins` can add providers later.

## Decisions

### D1. Resolver in `rustman-core`, invoked at the interpolation boundary
- Interpolation already produces a list of placeholders before sending. `secret:` placeholders and `{secret: …}` environment variables are collected there and resolved together.
- Concurrent fetches within a request are capped at 4, and each value then registers with the redaction set.
- This guarantees the "Resolution at send time only" requirement structurally: there's no resolver call anywhere else.
- Rejected: resolving when the environment loads, which would contact providers just to open the TUI.

### D2. Provider trait with async `fetch(path, field) -> Secret<String>`
- Providers return the `Secret<T>` type from the core design, so values can't be logged by accident.
- Errors are typed: `NotFound`, `FieldNotFound`, `Auth`, `Timeout`, `Unavailable` and `Config`. They map to the documented messages, and config errors give exit code 2 while the rest give exit code 3.
- The trait is object-safe, so `plugins` can provide implementations later.

### D3. REST over `rustman-http` for Vault and 1Password Connect; the `op` CLI by default
- The Vault and Connect HTTP APIs are small. Using `rustman-http` reuses TLS, proxy and CA settings and keeps the binary free of `vaultrs` and its extra stack.
- The `op` CLI is the only way to use biometric and desktop-app integration, so it's the 1Password default. Connect is for CI.
- Rejected: `vaultrs` (a second HTTP client, and a duplicate TLS config), and the 1Password SDKs (FFI to a native core).

### D4. Cloud providers behind features, with official credential chains
- `secrets-aws` uses `aws-config` + `aws-sdk-secretsmanager` + `aws-sdk-ssm` (rustls).
- `secrets-gcp` uses `gcp_auth` for ADC and REST through `rustman-http`.
- `secrets-azure` uses `azure_identity` for `DefaultAzureCredential` and REST through `rustman-http`.
- All of these are pure Rust on rustls, so there's no system library. They add compile time and size, so plain `cargo build` excludes them, and `cargo dist` release builds enable all three.
- Rejected: reimplementing the credential chains (SSO, IMDS, workload identity), which is a correctness and security risk.

### D5. SOPS: age native, PGP via `gpg`
- We parse the SOPS file format directly (an encrypted data key per recipient, plus AES-256-GCM per value) and decrypt age stanzas with the `age` crate.
- PGP support would pull in `sequoia` (which needs nettle or C crypto) or `rpgp` (heavy). We shell out to `gpg --decrypt` only for PGP stanzas, and only when `gpg` is on `PATH`.
- The MAC is verified before any value is returned.

### D6. Workspace can't cause command execution
- `cmd` providers and binary-path overrides (`op_path`, `gpg_path`) are accepted only from the user config.
- A cloned workspace is untrusted input (the same threat model as `plugins` and the `mcp-client` stdio trust). Letting `rustman.toml` name a command would turn a `git pull` into code execution.
- Rejected: a trust prompt for workspace-defined commands. That would add one more trust surface for a niche feature.

### D7. Cache
- `moka`-style per-key TTL with single-flight. Values are stored as `Secret<String>` and zeroized on eviction and on drop (`zeroize`).
- No on-disk layer at all.
- The TUI exposes a "Refresh secrets" palette action, which clears the cache.

### D8. Redaction label
- The label is the reference text (`provider/path#field`), or the environment variable name when the value backs one.
- This keeps redacted output informative ("which secret was this?") without revealing anything.

## Risks / Trade-offs

- [Provider latency slows the first request of a run] → Single-flight plus the TTL cache. `rustman secret test` lets CI fail fast before a long run.
- [Values in process memory could end up in a core dump] → `zeroize` on drop. Recommend disabling core dumps in CI. Accepted residual risk.
- [Release builds get bigger because of the AWS SDK] → Measured in CI, with a size budget. Users who care can build a minimal binary with `--no-default-features`.
- [Credential-key detection is heuristic] → An allowlist of known non-secret keys per provider type. Unknown keys trigger the generic "unknown key" warning from `workspace`.

## Migration Plan

Additive. Existing `secrets:` lists and `rustman secret set` keep working. Teams migrate by replacing environment values with `{secret: "…"}` entries. Rollback means reverting those entries.

## Open Questions

- Whether to support Vault dynamic secret engines (database credentials) with lease renewal, beyond reading them once. This is additive to the provider type.
