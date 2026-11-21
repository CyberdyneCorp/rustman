# Proposal

## Why

Writing request files by hand is the slowest part of adopting Rustman for an existing app. Teams want to answer "what does this app actually send?" and turn that real traffic into a versioned collection. A local recording proxy does both, without browser extensions or a cloud service.

## What Changes

- New `rustman record` command: a local HTTP(S) forward proxy bound to `127.0.0.1:8888` by default, plus a reverse-proxy mode (`--upstream <url>`) for clients that can't use a proxy.
- HTTPS interception only for hosts in an include list, using a per-workspace local CA. The CA is generated on request, its private key lives in the OS keychain, and it is never trusted automatically. Other hosts pass through as opaque CONNECT tunnels.
- `rustman record ca install | uninstall | export | rotate | remove` with explicit warnings.
- Filters by host glob, method, content type, and a default "drop static assets" rule.
- A live capture list in the TUI (a pane registered through the `tui` extension point) and JSON-lines output in the CLI.
- Saving captures as request files: dedupe by method + templated path, ids parameterized into variables, base URL extracted into an environment, and detected secrets turned into secret placeholders (never written raw).
- HAR 1.2 export, size limits, and clear reporting of certificate-pinning failures.

Out of scope: system-wide proxy configuration (Rustman prints instructions instead), HTTP/3/QUIC interception, rewriting or modifying traffic in flight (breakpoints), mobile-device provisioning, and sharing the CA across machines.

## Capabilities

### New Capabilities
- `traffic-recorder`: recording proxy (forward and reverse), local CA lifecycle, interception scope, filters, capture store, conversion to request files with secret placeholders, and HAR export.

### Modified Capabilities
None. It reuses `workspace` (request files, environments), `environments` (secrets, redaction), `request-history` (captures can be saved as exchanges) and `tui` (pane extension point) unchanged.

## Impact

- New crate `rustman-proxy` (hyper 1.x server and client, `rcgen` for certificates, `tokio-rustls`).
- New CLI subcommand tree `rustman record …` in `rustman-cli`.
- The local CA's private key is stored in the OS keychain via the `keyring` integration from `environments`.
- New `[record]` section in `rustman.toml`.
- Security-sensitive: it creates a certificate authority the user may choose to trust. See design.md, Risks.
