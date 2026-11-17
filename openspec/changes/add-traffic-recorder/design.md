# Design

## Context

See proposal.md, Why. The recorder is Rustman's second server component, after `webhook-inspector`. It follows the same rule: hyper does all HTTP parsing, with no hand-rolled framing (the Ghostman lesson in ROADMAP). Unlike the webhook inspector, it also originates TLS, because it relays to real upstreams, and it terminates TLS with a certificate authority it generates. That makes it the most security-sensitive feature after `evm-signing`.

## Goals / Non-Goals

**Goals:**
- One command that records an app's API traffic with no decryption of anything the user didn't explicitly include.
- A saved collection that's ready to commit: deduplicated, parameterized, and free of raw secrets.
- Same redaction and limits behavior as the rest of Rustman.

**Non-Goals:**
- Traffic modification (breakpoints, rewrite rules, throttling). That could come later in its own change.
- Configuring OS or browser proxy settings automatically.
- HTTP/3. Clients that try QUIC fall back to TCP when the proxy is set, and that's sufficient.

## Decisions

### D1. hyper 1.x for both proxy legs, `tokio-rustls` for TLS (rejected: `hudsucker`, `mitmproxy` sidecar)
`hudsucker` is a ready-made MITM proxy crate, but it pins its own hyper/rustls versions and its own CA handling, which we need to control (keychain storage, scope rules, rotation). A Python `mitmproxy` sidecar breaks the single-binary rule. The proxy is about 600 lines on top of hyper's `server::conn::http1`/`http2` plus `hyper::upgrade` for `CONNECT`. The upstream leg reuses `rustman-http`'s connector, so TLS verification, proxies and timings behave exactly as in `http-requests`.

### D2. Certificates with `rcgen` (rejected: `openssl` crate)
`rcgen` is pure Rust, so the "no system libraries" rule holds. The CA is ECDSA P-256 with `basicConstraints CA:true, pathlen:0`, `keyUsage keyCertSign`, `nameConstraints` absent (clients handle them inconsistently), and 1-year validity. Leaf certificates are minted per SNI host on demand with a 7-day validity, and cached in memory in an LRU of 1000 entries for the session only. Leaf keys never touch disk.

### D3. CA storage split
- **Private key:** OS keychain entry `rustman/record-ca/<workspace-id>`, using the per-clone workspace id introduced in `bootstrap-core-engine`. No keychain means no interception. We deliberately don't fall back to a file, since a leaked CA key lets an attacker impersonate any site to that machine.
- **Certificate:** `<config_dir>/rustman/record-ca/<workspace-id>.pem`, public data.
- **Trust installation:** shells out to the platform tool with the exact command printed first (`security add-trusted-cert` on macOS, `certutil -addstore` on Windows, `update-ca-certificates` or `trust anchor` on Linux). Rustman records which stores it touched in `<config_dir>/rustman/record-ca/<workspace-id>.installed`, so `uninstall` and `rotate` can undo exactly what was done. Firefox's NSS store is documented but not managed.

### D4. Scope first, decryption second
`CONNECT host:port` is matched against the include globs before any TLS handshake. Non-matching hosts get `hyper::upgrade` plus `tokio::io::copy_bidirectional`, a byte relay with nothing decrypted. This is the main privacy property: banking or personal traffic in the same browser stays opaque even while recording.

### D5. Capture model and conversion pipeline
A capture keeps the full exchange in memory (unredacted). Every outward path (pane, JSON lines, HAR, logs) goes through `environments`' single redaction step. Saving runs a pipeline:
1. filter
2. templatize the path (id heuristics: integer, UUID, ObjectId, ULID)
3. group by method and templated path, keeping the most recent body as the example
4. extract the base URL into the environment
5. detect secrets (header names, field names, plus a high-entropy check over 32 characters for unknown names, which emits a warning)
6. emit through `workspace`'s deterministic writer

Using the same writer means re-saving produces no diff.

### D6. Reverse mode shares the pipeline
`--upstream` swaps the accept loop. Requests arrive as origin-form, and the proxy forwards them to the fixed upstream. Everything after accept is shared code.

## Risks / Trade-offs

- [The user trusts a CA, which becomes a long-lived interception capability] → The CA is created only on request and installed only with typed confirmation. Its key never leaves the keychain, `pathlen:0`, 1-year expiry, and `rotate`/`uninstall` are one command. The TUI status bar shows `CA TRUSTED` while installed.
- [Recording other people's traffic with `--allow-remote`] → It's off by default, needs a flag plus a warning (and a confirmation in the TUI), and the listen address is printed prominently.
- [Secret detection misses an unusual field name] → The entropy heuristic warns about suspicious values. Saving prints a summary of the values it parameterized, and `--store-secrets` is opt-in. Users review the diff before committing, and that's documented.
- [Memory growth from long sessions] → `max_captures` and `max_buffer_bytes` bounds, plus body truncation.
- [Certificate pinning makes some apps unrecordable] → A clear `TLS REJECTED` hint. Excluding the host still lets the app work.

## Migration Plan

Additive. The new `[record]` section in `rustman.toml` has defaults. `rustman init` adds nothing to the workspace, because the CA lives outside it. Rollback means `rustman record ca uninstall`, plus deleting the binary feature if needed.

## Open Questions

- Whether to offer a `--browser` helper that launches Chromium with a throwaway profile already pointed at the proxy and CA. It's UX only and doesn't change the specs.
