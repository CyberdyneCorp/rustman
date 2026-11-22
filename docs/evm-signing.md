# EVM signing

`kind: evm-tx` requests are the only way Rustman signs or sends a transaction. Every one goes through the signing gate of the `rustman-evm-signing` crate; the agent interface (`rustman-mcp`) does not depend on that crate, so agents can never reach a signer.

This page covers signer configuration (keystore and OS keychain; hardware wallets are not available yet), the refusal of raw private keys, the gate preconditions, fees, gas and nonces, the confirmation prompt, sending, the receipt wait and the audit record that `rustman evm send` produces, and signing in `rustman run --sign`. A plain `rustman run` skips `evm-tx` files as `skipped (signing not requested)`, which is not a failure.

Signing happens on the command line only (`--sign`). The TUI does not sign in this release: an `evm-tx` file opens read-only there, with a notice pointing to `rustman evm send --sign` and `rustman run --sign`. TUI signing is tracked in [#112](https://github.com/CyberdyneCorp/rustman/issues/112).

## Opting in

Signing is off by default. A workspace opts in in `rustman.toml`:

```toml
[evm]
allow_signing = true
```

Without it, `rustman evm send` refuses before reading anything else:

```text
error: signing is disabled for this workspace; set [evm] allow_signing = true
```

## Signers

Signers are configured in `[evm.signers.<id>]`. Each is exactly one of:

```toml
[evm.signers.deployer]
keystore = "~/.foundry/keystores/deployer"   # Web3 Secret Storage JSON (Foundry, geth, MetaMask exports)
password = "{{secret:deployer_pw}}"          # optional; prompted (no echo) on a terminal otherwise

[evm.signers.ci]
keychain = true                              # stored by `rustman evm signer import ci`

```

A signer id is 1 to 64 letters, digits, `-` and `_`, and not hex-like (no `0x` prefix, not 32 or more hex digits). A request picks its signer with `signer: <id>`; `--signer <id>` (or the `RUSTMAN_EVM_SIGNER` environment variable) overrides it. An unknown id is a configuration error (exit code 2) listing the configured ones, and an invalid one is refused the same way; neither error repeats the value given, since a mistyped key could be in it:

```text
error: unknown signer (configured: ci, deployer)
```

### Keystore signers

The keystore is decrypted in memory only when the gate needs the signer, after the chain check. The file is read once (at most 64 KiB) and its key derivation is checked before it runs: `kdf` must be `scrypt` (n <= 2^18, r <= 8, p <= 4, n × r × p <= 2^21: geth's standard n = 2^18, r = 8, p = 1 is the ceiling) or `pbkdf2` (c <= 10 000 000) with matching parameters and `dklen` 32, so a hostile keystore cannot hang the process; standard Foundry, geth and MetaMask keystores are well inside. Anything else is refused as a configuration error (exit code 2). `~/` is the home directory; a relative path starts at the workspace root. The `password` is resolved like any variable, so keep it in a secret (a `{{secret:provider/path}}` reference or a variable listed in the environment's `secrets`); its value is redacted from every output. Without `password`, the password is asked on the terminal with no echo (a terminal that cannot be read is reported with its I/O error, exit code 2); without a terminal, the gate refuses:

```text
error: signer 'deployer': the keystore password is needed; set password = "{{secret:...}}" in [evm.signers.deployer] or run in a terminal
```

A wrong password is refused with `signer '<id>': wrong keystore password` (exit code 4).

### Keychain signers

```sh
rustman evm signer import ci          # no-echo prompt
rustman evm signer import ci < ci.key # or the first line of stdin
```

The key (64 hex digits, `0x` optional) is stored in the OS keychain (macOS Keychain, Windows Credential Manager, the Secret Service on Linux) under the `rustman` service as `evm-signer/<id>`, and the command prints the address and the `[evm.signers.<id>] keychain = true` lines to add. The key is never printed or written to a file: with `RUSTMAN_SECRET_STORE=file`, or where there is no keychain, the import fails with `no OS keychain is available; signer keys are never stored in files`.

### Hardware wallets

Hardware-wallet signers (Ledger, Trezor) are not available yet. A `ledger = "m/44'/60'/0'/0/0"` or `trezor = "..."` signer is read from `rustman.toml` but refused when used, in every build (the `hw-wallet` cargo feature is reserved for the drivers and links none yet), with a configuration error (exit code 2) and nothing signed or sent:

```text
error: signer 'cold': Ledger signing is not available yet; use a keystore or keychain signer
```

Hardware-wallet support is tracked in [#111](https://github.com/CyberdyneCorp/rustman/issues/111).

### Raw private keys are refused

Anything that looks like key material is refused wherever a signer value can come from: a run of 60 or more hex digits (with or without `0x`, so a key with a digit missing or extra padding too) and a seed phrase (12, 15, 18, 21 or 24 lowercase words). The places checked are the request's `signer`, an environment variable, a `--var`, `--signer`, `RUSTMAN_EVM_SIGNER`, any value in `[evm.signers]` (and any `private_key`, `key`, `secret_key`, `mnemonic` or `seed` field there), and the id given to `rustman evm signer import`. The refusal never echoes the key:

```text
error: raw private keys are not accepted; import the key with 'rustman evm signer import'
```

Key material only lives in memory inside `Secret` holders and is zeroized when dropped: decrypted keystore bytes, keychain entries, passwords and the signing key itself.

## The gate

`rustman evm send <request>` (and `rustman run --sign` for each `evm-tx` file) checks, in this order, and stops at the first refusal (exit code 4 unless noted):

1. `[evm] allow_signing = true`.
2. No key material as the request's `signer`, `--signer` or `RUSTMAN_EVM_SIGNER`.
3. `--sign`. Without it, the decoded preview is printed and the command refuses with `add --sign to sign and send`, before any signer is looked up. The preview names the signer only when it is a configured id (`(not a configured signer)` otherwise), and every value stays on its line (newlines and control characters show as `\u{…}`):

   ```text
   evm-tx transfer
     to      0x5FbDB2315678afecb367f032d93F642f64180aa3
     call    transfer(0x70997970C51812dc3A010C7d01b50e0d17dc79C8, 10)
     value   0
     signer  deployer
   error: add --sign to sign and send
   ```

4. A valid signer id configured in `[evm.signers]` (exit code 2 when missing, invalid or unknown).
5. The environment pins `chain_id`: `signing requires 'chain_id' in environment '<env>'` otherwise (or `signing requires an environment with 'chain_id'; select one with --env` without an environment).
6. The node's `eth_chainId`, asked again right before signing, equals the pin: `chain mismatch: environment 'dev' expects 31337 (Anvil/Hardhat local), endpoint reports 11155111 (Sepolia)` otherwise.
7. The transaction can be confirmed: a terminal, or `--yes-local` on a local chain (see [Confirmation](#confirmation)). Without one the gate refuses here, before the key is unlocked or anything simulated.
8. The signer unlocks.
9. The simulation, `eth_call` from the signer at `pending` and `eth_estimateGas`, succeeds. A revert is shown decoded and refused:

   ```text
   error: simulation reverted: InsufficientBalance(available=5, required=10); nothing was signed or sent (add --force-revert to sign anyway)
   ```

   With `--force-revert` the gate goes on, prints `warning: SIMULATION REVERTED: ...`, and the confirmation starts with the same warning. A reverting call has no gas estimate, so a forced transaction needs `gas` in the request.

10. The confirmation (see below).
11. `eth_chainId` once more, immediately before signing.

Nothing is signed or sent by a refusal, and loading a workspace with `evm-tx` files never calls a node or touches a signer. Ctrl+C at any prompt, or anywhere before the transaction is sent, stops with `interrupted; nothing was signed or sent` and exit code 130; the gate checks once more right after the confirmation and right before sending.

## Fees, gas and nonces

Values set in the request win; the rest are suggested by the node and labelled `(suggested)` in the confirmation. Fees take every quantity form (`30 gwei`, `0x6fc23ac00`, wei as a decimal integer).

- **EIP-1559** (the default, `type: eip1559`): `eth_feeHistory(10, "latest", [50])`. `max_priority_fee` is the median of the 50th-percentile rewards of those blocks; `max_fee` is 2 × the latest base fee + the priority fee. A node that reports no base fee is an error asking for `type: legacy`. `gas_price` is refused on an EIP-1559 transaction, and a `max_priority_fee` above `max_fee` is refused.
- **Legacy** (`type: legacy`): `gas_price`, by default `eth_gasPrice`; EIP-155 replay protection embeds the chain id. `max_fee` and `max_priority_fee` are refused.
- **Gas limit**: `gas`, by default the simulation's `eth_estimateGas` × 1.2, rounded up.
- **Ceilings**: a node cannot inflate what is signed. A suggested `max_fee`, `max_priority_fee` or `gas_price` above 10 000 gwei, or a gas limit from the estimate above 30 000 000, is refused (exit code 4) with a message asking to set the value in the request; values set in the request are not capped.
- **Nonce**: `nonce`, by default `eth_getTransactionCount(signer, "pending")`. Within one process (one `rustman evm send`, or a whole `rustman run --sign`), later transactions of the same signer on the same chain count up from the last one sent instead of asking again. A `nonce too low` or `replacement underpriced` answer is reported as the node sent it (exit code 3) and nothing is retried.

## Confirmation

Before signing, the decoded summary is printed:

```text
  chain        Sepolia (11155111)
  signer       0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 (deployer)
  to           0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238 (usdc.eth)
  call         transfer(to=0x70997970C51812dc3A010C7d01b50e0d17dc79C8, amount=10.00 USDC (10000000))
  value        0 ETH
  gas          55317
  max fee      16.5 gwei (suggested)
  priority fee 1.5 gwei (suggested)
  nonce        4
  max cost     0.0009127305 ETH
sign and send this transaction? [y/N]
```

The amount of an ERC-20 `transfer`, `approve` or `transferFrom` is shown in token units with the token's `decimals()` and `symbol()` when the token answers them, and the raw integer stays in parentheses (the decimals come from the node). `max cost` is the value plus the gas limit at the max fee (or gas price). Only `y` or `yes` goes on; anything else cancels with `cancelled; nothing was signed or sent` (exit code 4).

- **Chain 1 and `[evm] protected_chains`**: after `y`, the chain name must be typed exactly (`type the chain name (Ethereum Mainnet) to confirm:`). Anything else cancels with `the chain name did not match 'Ethereum Mainnet'; cancelled; nothing was signed or sent`. Chain 1 and the other built-in chains always use their built-in name; a protected chain configured in `[evm.chains.<id>]` without a name is never confirmed (`protected chain <id> has no name to type; set [evm.chains.<id>] name`).
- **No terminal** (CI, a pipe): signing is allowed only on chain 31337 and the chain ids of `[evm] headless_signing_chains`, and only with `--yes-local`, which confirms without a prompt (`confirmed by --yes-local on Anvil/Hardhat local (31337)`). Elsewhere:

  ```text
  error: signing on Sepolia (11155111) needs an interactive confirmation; without a terminal only chain 31337 and [evm] headless_signing_chains can be signed on, with --yes-local
  ```

  On a local chain without `--yes-local`: `no terminal to confirm on; add --yes-local to sign on Anvil/Hardhat local (31337) without a prompt`.
- `--yes-local` never skips the prompt of another chain: on a terminal it prints a note and asks as usual.

```toml
[evm]
allow_signing = true
protected_chains = [8453]            # Base: type its name too
headless_signing_chains = [1337]     # another local devnet CI may sign on
```

Every value of the summary, and the chain name in the typed-name question, is kept on one line: newlines, tabs, control, bidi and zero-width characters show as `\u{…}`.

## Sending and the receipt

The signed transaction is sent with `eth_sendRawTransaction`, the one place that method is allowed (every `jsonrpc` request, `evm-call` and agent call is refused it). The hash is printed (`sent 0x…`), then the receipt is awaited:

- `--confirmations N` (default 1, the block that includes it) blocks must be on top of the head;
- `--receipt-timeout` (default `120s`): when it passes, the command prints `error: transaction 0x… was sent but not mined within 120s` and the explorer link (when the chain has an explorer), and exits with code 3. The transaction may still be mined later: check it with `rustman evm tx <hash>`.
- A receipt poll that fails (the node is down, rate-limited, ...) is tried again, waiting twice as long after each failure in a row (up to 5 s), until the receipt timeout. When the last poll still failed, the command prints `error: transaction 0x… was sent but its receipt could not be read within 120s: <the node's error>` and the explorer link, and exits with code 3; the same happens when the mined transaction cannot be decoded. The audit record is written either way, with `"status": "receipt-unknown"`.

The mined transaction is printed decoded, as `rustman evm tx` shows it, with `explorer: https://…/tx/0x…`. A transaction that reverted on chain exits with code 1.

### Receipt assertions and captures

The request's `assertions` and `captures` run on the receipt:

```yaml
assertions:
  - evm: {emits: "Transfer(address,address,uint256)", where: {to: "{{bob}}"}}
  - evm: {gas_lt: 80000}
  - json: {path: $.receipt.status, equals: success}
captures:
  - {var: mined_in, from: evm_receipt, field: blockNumber}
  - {var: moved, from: evm_event, event: "Transfer(address,address,uint256)", field: value}
```

`emits` and `evm_event` read the receipt's logs decoded with the request's ABI; `gas_lt` reads the gas used; `evm_receipt` reads `contractAddress`, `blockNumber`, `status` (`success` or `reverted`) and `gasUsed`. A failed assertion prints `fail: ...` and exits with code 1.

### Audit record

Every sent transaction is recorded in `request-history` (unless `--no-history` or `[history] enabled = false`), also when the receipt timed out (`"status": "pending"`) or could not be read (`"status": "receipt-unknown"`). The entry's `record.evm` holds:

```json
{
  "kind": "evm-tx", "chain_id": 31337, "chain": "Anvil/Hardhat local",
  "signer": {"id": "deployer", "address": "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"},
  "hash": "0x…", "to": "0x…", "to_name": null, "call": "add(amount=2)",
  "value": "0", "nonce": 0, "gas_limit": "55317",
  "fees": {"type": "eip1559", "max_fee": "…", "max_priority_fee": "…", "suggested": true},
  "forced_revert": false, "status": "success", "block": 1,
  "gas_used": "…", "effective_gas_price": "…", "fee": "…", "explorer": null
}
```

The signed bytes, the signature, the password and the key are never stored or shown, and the entry cannot be replayed (`rustman history replay` would not sign again).

| Exit code | Meaning |
| --- | --- |
| 0 | Sent, mined, and every assertion passed. |
| 1 | Mined, but it reverted on chain or an assertion failed. |
| 2 | A configuration error: an unknown or invalid signer, a missing keystore, a keystore above the key-derivation limits, an unreadable password prompt, no keychain, a hardware-wallet signer. |
| 3 | The node could not be reached or refused the transaction (`nonce too low`, ...), or the transaction was sent but its receipt did not come within `--receipt-timeout` or could not be read. |
| 4 | A gate refusal: signing disabled, key material as a signer, no `--sign`, no `chain_id` pin, a chain mismatch, no way to confirm (no terminal on a non-local chain), a wrong or missing keystore password, a reverting simulation, a declined confirmation or a wrong chain name. |
| 130 | Ctrl+C before the transaction was sent; nothing was signed or sent. |

## In `rustman run`

`rustman run` skips `evm-tx` files unless `--sign` is passed; they are reported as `skipped (signing not requested)` and are not failures. With `--sign`, each `evm-tx` file of the run goes through the same gate as `rustman evm send`, one at a time and in run order:

```sh
rustman run . --env anvil --sign --yes-local
```

- The signer is the request's `signer`, or `RUSTMAN_EVM_SIGNER`.
- The preview, the summary and the receipt go to stderr; stdout is the run's report.
- `--yes-local` (which requires `--sign`) confirms without a prompt on chain 31337 and `[evm] headless_signing_chains` only. Without a terminal, every `evm-tx` file on another chain is refused, and the run exits with code 4. On a terminal, each transaction is confirmed as with `rustman evm send`.
- Nonces count up across the run: two transactions of one signer use the pending count, then that count + 1, without asking the node again.
- A refusal is the request's error and makes the run exit with code 4; a receipt timeout or an unreadable receipt is an error (exit code 3), with the audit record kept; Ctrl+C before a transaction is sent skips it as interrupted (exit code 130); a failed receipt assertion or a transaction that reverted on chain fails the request (exit code 1). The receipt timeout is 120 s and one confirmation is awaited.

## Agents can never sign

The agent interface (`rustman mcp serve`) has no tool that signs, sends a transaction, imports a key or changes signing configuration, and `rustman-mcp` does not link the `rustman-evm-signing` crate at all: a test walks `Cargo.lock` and fails if anything the agent crate depends on reaches it. An agent that names a saved `evm-tx` file in `send_request` is refused before anything is resolved or sent:

```text
evm-tx requests cannot be sent through the agent interface
```

`run_collection` skips `evm-tx` files like `rustman run` does, and `rpc_call` refuses `eth_sendRawTransaction` and the other sending methods (see [send methods](evm.md#send-methods)).

## CI on Anvil

A pipeline can sign on a local Anvil chain without a terminal: chain 31337 with `--yes-local`, a keystore of one of Anvil's dev accounts, and its password from the CI's secrets through the `env` provider ([secret providers](secrets.md#env)). The files below are the ones Rustman's own CI runs (`crates/rustman-cli/tests/docs_evm_signing.rs`, in the `jsonrpc-anvil` job).

```toml
# rustman.toml
[workspace]
name = "counter"
version = 1

[secret_providers.ci]
type = "env"

[evm]
allow_signing = true

[evm.signers.deployer]
keystore = "keys/anvil0.json"
password = "{{secret:ci/DEPLOYER_PW}}"
```

```yaml
# environments/anvil.yaml
chain_id: 31337
variables:
  rpc: http://127.0.0.1:8545
```

```yaml
# add.rman.yaml
version: 1
kind: evm-tx
name: add
rpc: "{{rpc}}"
to: "0x5FbDB2315678afecb367f032d93F642f64180aa3"
abi: Counter.abi.json
function: add
args: ["2"]
signer: deployer
assertions:
  - evm: {emits: "Added(address,uint256,uint256)", where: {amount: "2"}}
  - evm: {gas_lt: 100000}
captures:
  - {var: mined_in, from: evm_receipt, field: blockNumber}
```

`keys/anvil0.json` is Anvil's first dev account (`0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266`) encrypted with the password in `DEPLOYER_PW`, made once with `cast wallet import anvil0 --interactive` (it lands in `~/.foundry/keystores/anvil0`). That key is public: use it on a local chain only, and never a real key in CI. `Counter.abi.json` is the ABI of a contract with `add(uint256)` that emits `Added(address indexed by, uint256 amount, uint256 number)`; `0x5FbDB2315678afecb367f032d93F642f64180aa3` is where the first deployment of that account lands on a fresh Anvil. Deploy with Foundry or Hardhat (deployment is out of Rustman's scope); Rustman's own test installs the contract's runtime code there with `anvil_setCode` instead.

The CI steps, with `DEPLOYER_PW` from the CI's secrets:

```sh
anvil --port 8545 &
rustman evm send add --env anvil
rustman evm send add --env anvil --sign --yes-local
rustman evm send add --env anvil --sign --yes-local --confirmations 1
rustman run . --env anvil --sign --yes-local
```

The first `send` prints the decoded preview and exits with code 4 (no `--sign`); the next two sign without a prompt (`confirmed by --yes-local on Anvil/Hardhat local (31337)`), with nonces 0 and 1, wait for the receipt, check the `Added` event and the gas, and exit with code 0. The `run` signs the workspace's `evm-tx` files the same way (nonce 2) and exits with code 0. Each sent transaction is in the history's audit record; the password, the key and the signed bytes are in none of the output or files. The same files with an environment whose `chain_id` is not 31337 (or in `[evm] headless_signing_chains`) are refused without a terminal, so a CI job cannot sign on a real network by accident.
