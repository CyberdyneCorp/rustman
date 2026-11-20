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
- Errors are typed: `NotFound`, `FieldNotFound`, `Auth`, `Timeout`, `Unavailable` and `Config`. They map to the documented messages. Configuration problems found when the configuration loads give exit code 2; any failure while resolving a reference, a `Config` one (a type compiled out, a Vault without an address) included, is an unresolved secret with exit code 3, in `send`, `run` and `secret get` alike. `secret test` exits 0 or 3, an unknown provider name included.
- The trait is object-safe, so `plugins` can provide implementations later.

### D3. REST over `rustman-http` for Vault and 1Password Connect; the `op` CLI by default
- The Vault and Connect HTTP APIs are small. Using `rustman-http` reuses its engine and TLS stack (verifying against the system trust store, with direct connections; see D4) and keeps the binary free of `vaultrs` and its extra stack.
- The `op` CLI is the only way to use biometric and desktop-app integration, so it's the 1Password default. Connect is for CI.
- Rejected: `vaultrs` (a second HTTP client, and a duplicate TLS config), and the 1Password SDKs (FFI to a native core).

### D4. Cloud providers behind features, with official credential chains
- The cloud types are thin REST clients on `rustman-http`, like Vault and Connect (D3), so they reuse its engine and TLS verification against the operating system's trust store. They connect directly: the workspace's CA files, client certificates, proxy and `insecure` settings are not applied, so a cloned workspace cannot weaken them (a private CA must be in the system trust store; a proxy-only network is not supported yet):
  - `secrets-aws`: Secrets Manager `GetSecretValue` and SSM `GetParameter` over the AWS JSON protocol, signed by the engine's SigV4 signer (from runner parity).
  - `secrets-gcp`: Secret Manager's `versions/<v>:access`, with a token from Application Default Credentials (a service account key signed with `jsonwebtoken` on ring, gcloud's user credentials, the metadata server).
  - `secrets-azure`: Key Vault's `secrets/<name>[/<version>]`, with a token from `DefaultAzureCredential`'s chain (service principal, workload identity, managed identity, the Azure CLI).
- Each credential chain follows the platform's documented order. Metadata endpoints get a 1 s timeout, and expiring credentials are held in memory until 5 minutes before they expire.
- The features add no crate that is not already in the tree (`time` parsing aside), so plain `cargo build` still excludes them only to keep the default build minimal; release builds enable all three (`rustman-cli` feature `cloud-secrets`). A CI job checks every type is listed and that they add at most 2 MiB.
- `endpoint` (an emulator such as LocalStack) is accepted from the user config only, since it would let a workspace send cloud credentials to a host of its choice.
- A Vault `address` or 1Password `connect_host` from `rustman.toml` stays allowed (the spec's "Valid Vault config"), but the user's own credentials (`VAULT_TOKEN`, `~/.vault-token`, AppRole ids, `OP_CONNECT_TOKEN`) are sent there only when `VAULT_ADDR`/`OP_CONNECT_HOST` names the same host or the user config sets it; an OIDC login is allowed since its token comes from that server. AWS `region` must match `[a-z0-9-]+`, since it is part of the endpoint host.
- Rejected: the AWS, Google and Azure SDKs (`aws-config`, `gcp_auth`, `azure_identity`). They bring `reqwest`, `aws-lc-rs` or native TLS, which `cargo deny` bans, plus a second HTTP stack. Reimplementing the chains is the risk this accepts: they are kept to the documented sources, and SSO, assume-role and `credential_process` AWS profiles are refused with a hint rather than half-supported.

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

### D9. Agents send references, never write them
- An agent may send saved requests whose files (or environments) use references, but every MCP tool call whose arguments contain a `{{secret:...}}` placeholder is refused before dispatch. Otherwise a prompt-injected agent could pick any secret (another environment's keychain entry, any Vault path, the user's `cmd` provider) and choose where it is sent.
- One check over all arguments covers every ad-hoc shape (HTTP, JSON-RPC, realtime, gRPC, `vars`) and future tools.
- Values inserted from variables and captures are never parsed again, so they cannot carry a reference.
- Rejected: allowing only references that already appear in the workspace's files. It still lets the agent send a known secret to a host of its choice, and needs a scan of every file.

## Risks / Trade-offs

- [Provider latency slows the first request of a run] → Single-flight plus the TTL cache. `rustman secret test` lets CI fail fast before a long run.
- [Values in process memory could end up in a core dump] → `zeroize` on drop. Recommend disabling core dumps in CI. Accepted residual risk.
- [Release builds get bigger because of the AWS SDK] → Measured in CI, with a size budget. Users who care can build a minimal binary with `--no-default-features`.
- [Credential-key detection is heuristic] → An allowlist of known non-secret keys per provider type. Unknown keys trigger the generic "unknown key" warning from `workspace`.

## Migration Plan

Additive. Existing `secrets:` lists and `rustman secret set` keep working. Teams migrate by replacing environment values with `{secret: "…"}` entries. Rollback means reverting those entries.

## Open Questions

- Whether to support Vault dynamic secret engines (database credentials) with lease renewal, beyond reading them once. This is additive to the provider type.
