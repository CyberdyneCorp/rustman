# Rustman developer commands. Run `just` to list them.
#
# The recipes mirror what CI runs (.github/workflows/ci.yml), so `just ci`
# before pushing catches what a pull request would fail on.

set windows-shell := ["pwsh", "-NoLogo", "-NoProfile", "-Command"]

# Features of the official release builds (.github/workflows/release.yml).
release_features := "cloud-secrets,otlp,smtp"
openspec := "npx -y @fission-ai/openspec@2.1.0"

# List the recipes
default:
    @just --list --unsorted

# ── Build and run ───────────────────────────────────────────────────────────

# Debug build of the `rustman` binary
build:
    cargo build -p rustman-cli --locked

# Release build with the features of the official archives
release:
    cargo build --release -p rustman-cli --locked --features {{release_features}}

# Install `rustman` into ~/.cargo/bin from this checkout
install:
    cargo install --path crates/rustman-cli --locked --features {{release_features}}

# Run the CLI with arguments, e.g. `just run send GET https://example.com`
run *args:
    @cargo run -q -p rustman-cli --locked -- {{args}}

# Open the terminal UI on a workspace (default: the current directory)
tui dir=".":
    @cargo run -q -p rustman-cli --locked -- tui {{dir}}

# Run the CLI with debug logs and full backtraces
[unix]
debug *args:
    @RUST_LOG=${RUST_LOG:-debug} RUST_BACKTRACE=full cargo run -p rustman-cli --locked -- {{args}}

# Run the CLI with debug logs and full backtraces
[windows]
debug *args:
    $env:RUST_LOG = if ($env:RUST_LOG) { $env:RUST_LOG } else { "debug" }; $env:RUST_BACKTRACE = "full"; cargo run -p rustman-cli --locked -- {{args}}

# ── Lint ────────────────────────────────────────────────────────────────────

# Format the code
fmt:
    cargo fmt --all

# Check formatting, as CI does
fmt-check:
    cargo fmt --all --check

# Clippy with warnings as errors: default, CLI alone, and the optional features
clippy:
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo clippy -p rustman-cli --locked -- -D warnings
    cargo clippy --workspace --all-targets --locked --features rustman-cli/otlp,rustman-cli/cloud-secrets -- -D warnings
    cargo clippy -p rustman-monitor --all-targets --locked --features smtp -- -D warnings

# Check dependencies for advisories, licenses and bans (needs cargo-deny)
deny:
    cargo deny check

# Validate the OpenSpec specs and changes (needs Node.js)
openspec:
    {{openspec}} validate --all --strict

# Every static check CI runs: format, clippy, cargo-deny, OpenSpec
lint: fmt-check clippy deny openspec

# ── Test ────────────────────────────────────────────────────────────────────

# Workspace tests; extra arguments go to cargo, e.g. `just test -p rustman-core`
test *args:
    cargo test --workspace --locked {{args}}

# Tests of one crate, optionally filtered, e.g. `just test-crate rustman-tui summary`
test-crate crate filter="":
    cargo test -p {{crate}} --locked -- {{filter}}

# Workspace tests with the optional features of the official builds
test-features:
    cargo test --workspace --locked --features rustman-cli/otlp,rustman-cli/cloud-secrets
    cargo test -p rustman-monitor --features smtp --locked

# The minimal build: no default features
test-minimal:
    cargo build -p rustman-cli --no-default-features --locked
    cargo test -p rustman-cli --no-default-features --locked --bin rustman

# EVM tests against a local Anvil (needs Foundry: https://getfoundry.sh)
[unix]
test-anvil:
    RUSTMAN_REQUIRE_ANVIL=1 cargo test -p rustman-evm -p rustman-evm-signing -p rustman-cli --locked

# Terminal UI smoke tests in a pseudo-terminal
test-tui:
    cargo test -p rustman-tui --locked --test pty_smoke
    cargo test -p rustman-tui --locked --lib smoke_tests

# Review changed insta snapshots (needs cargo-insta)
snapshots:
    cargo insta review

# Regenerate the frozen public-contract fixtures (docs/stability.md) after an intended change
[unix]
freeze:
    RUSTMAN_FREEZE_REPORT=1 cargo test -p rustman-report --locked --test frozen
    RUSTMAN_FREEZE_VIZ=1 cargo test -p rustman-viz-protocol --locked --test frozen

# The executor overhead benchmark
bench:
    cargo bench -p rustman-bench --bench executor --locked

# Fuzz a target for a while (needs nightly and cargo-fuzz): `just fuzz sse` or `just fuzz viz`
fuzz target="sse" seconds="60":
    cd {{ if target == "viz" { "crates/rustman-viz-protocol" } else { "crates/rustman-http" } }} && cargo +nightly fuzz run {{ if target == "viz" { "decode" } else { "sse_parser" } }} -- -max_total_time={{seconds}}

# Scaffold, build and test every plugin template (needs the wasm32-wasip2 target)
plugin-templates:
    python3 scripts/check_plugin_templates.py

# ── Docs ────────────────────────────────────────────────────────────────────

# Check the documentation book: chapters, links and anchors
docs-check:
    python3 scripts/check_docs.py

# Serve the documentation book with live reload (needs mdbook)
docs-serve:
    mdbook serve --open

# ── Viz companion (its own workspace, links the Raven C library) ────────────

# Build the rustman-viz companion with Raven
viz:
    cd crates/rustman-viz && cargo build --release --locked --features raven

# Test the companion, with and without Raven
viz-test:
    cd crates/rustman-viz && cargo test --locked && cargo test --features raven --locked

# ── All together ────────────────────────────────────────────────────────────

# What a pull request must pass: static checks, then the workspace tests
ci: lint test

# Remove build artifacts
clean:
    cargo clean
