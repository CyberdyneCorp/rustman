# Install and quick start

There are no release binaries yet. Build from source with [rustup](https://rustup.rs); the pinned toolchain (see `rust-toolchain.toml`) is installed automatically. No system libraries are needed.

```bash
git clone https://github.com/CyberdyneCorp/rustman
cd rustman
cargo install --path crates/rustman-cli --locked
rustman --version
```

## A first workspace

A workspace is a folder with a `rustman.toml` at its root; request files, environments and collections are plain files inside it, meant to be committed to Git (see [Request files](request-files.md)).

```bash
rustman init my-api && cd my-api       # rustman.toml, environments/local.yaml, examples/ping.rman.yaml
rustman env list                       # '*' marks the active environment (--env > RUSTMAN_ENV > default)
rustman send examples/ping             # send one request file and print the response
rustman send GET https://httpbin.org/uuid --body-only   # or an ad-hoc request, no file needed
rustman run                            # run every request and check its assertions (exit 1 on a failure)
rustman run --reporter pretty --reporter junit:out/junit.xml
rustman history list --limit 5         # past exchanges, redacted
rustman export snippet --lang python examples/ping       # the request as Python code
rustman                                # open the terminal UI (or: rustman tui)
```

`rustman --help` lists every command, and each one has its own `--help`.

## Where to go next

- Coming from Postman or Newman: [Migrating from Postman](migrate-from-postman.md), then [Importing collections](import.md) for other tools.
- Working interactively: the [Terminal UI](tui.md).
- Scripting and CI: the [CLI reference](cli.md), [Assertions and captures](assertions.md), [Data-driven runs](data-runs.md) and [Reports](reports.md).
- Variables and credentials: [Environments, variables and secrets](environments.md) and [Secret providers](secrets.md).
- Letting an AI agent use the workspace: [Agents (MCP server)](agent.md).
