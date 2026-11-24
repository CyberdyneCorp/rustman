# Proposal

## Why

The HTTP engine review (issues #37, #38, #39, #41 and #45) found gaps in how response bodies and the OAuth2 login handle hostile or unusual conditions: outside a workspace, bodies spilled to a shared, predictable temp folder that another local user could pre-create; a body that is not decoded had no size limit, so an endless stream could fill the disk before the timeout; the OAuth2 loopback listener retried a failing `accept` in a busy loop and served any number of connections at once; and users were not told when their OAuth2 tokens landed in `.rustman/secrets.yaml` instead of the OS keychain.

## What Changes

- Outside a workspace, spilled bodies go to a per-user temp folder (`rustman-bodies-<uid>` on Unix). A spill folder that is a symbolic link or belongs to another user is refused, and spill files are always created new.
- A body that is not decoded is limited by `rustman.toml [http] max_download_bytes` (default 1 GiB), failing with `network: the response body is larger than <n> bytes`.
- The OAuth2 loopback listener serves at most 16 connections at once and backs off (10 ms doubling to 1 s) when `accept` fails.
- `rustman auth login` warns once when the token is stored in `.rustman/secrets.yaml`.
- The response-phase timeout test keeps the spec's bound in spirit with a margin that holds on loaded CI runners (test only).

Out of scope: the zstd-sys decision (#44), redirect header stripping (#40, its own change), workspace-level mTLS (#42) and the `ca_bundle` trust roots (#43).

## Capabilities

### New Capabilities
None.

### Modified Capabilities
- `http-requests`: response body limits and spill folder safety, and the OAuth2 loopback listener and token storage notice.

## Impact

- `rustman-core`: `secrets::ensure_private_dir` and `secrets::user_temp_dir`; `[http] max_download_bytes` in `rustman.toml`. New dependency on `rustix` (Unix only, `process` feature, already in the tree) for the safe `geteuid`.
- `rustman-http`: `BodyOptions::max_download`, `default_spill_dir`, the loopback listener limits, and `oauth2_login` returning where the token was stored.
- `rustman-cli`: the `auth login` warning.
- Docs: `docs/http.md`, `docs/environments.md`, `docs/request-files.md`.
