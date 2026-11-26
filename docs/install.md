# Installing Rustman

Rustman is one binary, `rustman`. Install a release archive, or build it from source.

## Release archives

Each [GitHub release](https://github.com/CyberdyneCorp/rustman/releases) has one archive per platform, plus a `SHA256SUMS` file:

| Platform | Archive |
|---|---|
| Linux x86_64 (static, any distribution) | `rustman-<version>-x86_64-unknown-linux-musl.tar.gz` |
| Linux ARM64 (static, any distribution) | `rustman-<version>-aarch64-unknown-linux-musl.tar.gz` |
| macOS Intel | `rustman-<version>-x86_64-apple-darwin.tar.gz` |
| macOS Apple silicon | `rustman-<version>-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `rustman-<version>-x86_64-pc-windows-msvc.zip` |

An archive holds a `rustman-<version>-<target>/` directory with the binary, `LICENSE-APACHE`, `LICENSE-MIT`, `README.md` and `completions/` (bash, zsh, fish, PowerShell and elvish scripts, the output of [`rustman completions`](cli.md#completions)). The Linux binaries are statically linked, and the Windows binary needs no Visual C++ redistributable.

### What the release binaries include

Release binaries are built with these cargo features of `rustman-cli`, on top of the defaults (WASM plugins):

| Feature | What it adds | Docs |
|---|---|---|
| `cloud-secrets` | The AWS, Google Cloud and Azure secret provider types | [Secret providers](secrets.md#cloud-providers) |
| `otlp` | OTLP export of client spans, off until `[trace.export]` turns it on | [Trace propagation](tracing.md#exporting-client-spans-otlp) |
| `smtp` | The `email` notification channel of monitors | [Monitors](monitors.md#notification-channels) |

Each one only does something when configured, and none needs a system library. Hardware-wallet signers (`hw-wallet`, Ledger and Trezor for `rustman evm send`) are not in release binaries; build from source with `--features hw-wallet` to get them.

### Download and verify

Every archive and `SHA256SUMS` carry a [build provenance attestation](https://docs.github.com/actions/security-for-github-actions/using-artifact-attestations) signed with Sigstore keyless signing: GitHub Actions signs it with a short-lived certificate bound to the release workflow run, so there is no long-lived key to trust or leak. Checking it proves the file was built by `.github/workflows/release.yml` in `CyberdyneCorp/rustman` from the tagged commit.

With the [GitHub CLI](https://cli.github.com) (2.49 or later):

```bash
version=1.0.1                                  # the release you want
target=aarch64-apple-darwin                    # your platform, from the table above
gh release download "v$version" --repo CyberdyneCorp/rustman \
  --pattern "rustman-$version-$target.tar.gz" --pattern SHA256SUMS

# 1. Provenance: built by this repository's release workflow.
gh attestation verify "rustman-$version-$target.tar.gz" --repo CyberdyneCorp/rustman

# 2. Checksum: the archive matches SHA256SUMS.
sha256sum --check --ignore-missing SHA256SUMS  # macOS: shasum -a 256 --check --ignore-missing SHA256SUMS
```

To tie the check to a release tag as well, add `--source-ref "refs/tags/v$version"` to `gh attestation verify`. `SHA256SUMS` itself can be verified the same way (`gh attestation verify SHA256SUMS --repo CyberdyneCorp/rustman`). Without the GitHub CLI, download the files from the release page and check the checksum; the attestation can also be verified offline with `cosign` or the Sigstore tooling from the bundle `gh attestation download` fetches.

On Windows (PowerShell):

```powershell
gh attestation verify rustman-1.0.1-x86_64-pc-windows-msvc.zip --repo CyberdyneCorp/rustman
(Get-FileHash rustman-1.0.1-x86_64-pc-windows-msvc.zip -Algorithm SHA256).Hash.ToLower()   # compare with SHA256SUMS
```

### Install

```bash
tar -xzf "rustman-$version-$target.tar.gz"
install -m 755 "rustman-$version-$target/rustman" ~/.local/bin/   # or any directory on your PATH
rustman --version
```

On Windows, extract the zip and put `rustman.exe` in a directory on your `PATH`. For shell completions, copy the script for your shell from `completions/` to where your shell looks for it (see [Completions](cli.md#completions)).

macOS marks files downloaded by a browser as quarantined, and Gatekeeper refuses binaries that are not notarized. Archives fetched with `gh release download` or `curl` are not quarantined; for one downloaded with a browser, run `xattr -d com.apple.quarantine rustman` after verifying it.

## From source

Build with [rustup](https://rustup.rs); the pinned toolchain (see `rust-toolchain.toml`) is installed automatically. No system libraries are needed.

```bash
git clone https://github.com/CyberdyneCorp/rustman
cd rustman
cargo install --path crates/rustman-cli --locked
rustman --version
```

To check out a release, add `--branch v<version>` to `git clone`. A plain build leaves out the optional features. To build what the release binaries ship:

```bash
cargo install --path crates/rustman-cli --locked --features cloud-secrets,otlp,smtp
```

`rustman-cli` is not published on crates.io, so `cargo install rustman-cli` does not work; `cargo install --locked --git https://github.com/CyberdyneCorp/rustman --tag v<version> rustman-cli` builds a release straight from the repository.

## Cutting a release (maintainers)

1. Set `version` in `[workspace.package]` and the `rustman-*` entries of `[workspace.dependencies]` in `Cargo.toml`, run `cargo check --workspace` so `Cargo.lock` follows, and merge that to `main`.
2. Tag the merged commit and push the tag: `git tag -s v1.0.0 -m "rustman 1.0.0" && git push origin v1.0.0`. The workflow fails if the tag is not `v` plus the workspace version.
3. The [Release workflow](https://github.com/CyberdyneCorp/rustman/blob/main/.github/workflows/release.yml) builds and smoke tests every target on its own runner, writes `SHA256SUMS`, attests every file and creates a **draft** release with generated notes.
4. Review the draft (notes, the five archives and `SHA256SUMS`), then publish it.

Running the workflow by hand (**Actions → Release → Run workflow**) is a dry run: it builds, smoke tests and attests, and never creates a release. The archives are kept as workflow artifacts.
