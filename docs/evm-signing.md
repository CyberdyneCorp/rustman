# EVM signing

`kind: evm-tx` requests are the only way Rustman signs or sends a transaction. Every one goes through the signing gate of the `rustman-evm-signing` crate; the agent interface (`rustman-mcp`) does not depend on that crate, so agents can never reach a signer.

This page covers signer configuration (keystore and OS keychain; hardware wallets are not available yet), the refusal of raw private keys, the gate preconditions, fees, gas and nonces, the confirmation prompt, sending, the receipt wait and the audit record that `rustman evm send` produces, signing in `rustman run --sign`, and signing from the TUI's `evm-tx` pane. A plain `rustman run` skips `evm-tx` files as `skipped (signing not requested)`, which is not a failure.

Every signature needs a per-invocation opt-in: `--sign` on the command line, or the `evm.sign` action (`Alt+T`) in the TUI (see [In the TUI](#in-the-tui)).

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

The keystore is decrypted in memory only when the gate needs the signer, after the chain check. The file is read once (at most 64 KiB) and its key derivation is checked before it runs: `kdf` must be `scrypt` (n <= 2^18, r <= 8, p <= 4, n × r × p <= 2^21: geth's standard n = 2^18, r = 8, p = 1 is the ceiling) or `pbkdf2` (c <= 10 000 000) with matching parameters and `dklen` 32, so a hostile keystore cannot hang the process; standard Foundry, geth and MetaMask keystores are well inside. Anything else is refused as a configuration error (exit code 2). The cipher is checked the same way: `cipher` must be `aes-128-ctr`, `cipherparams.iv` exactly 32 hex digits, `ciphertext` 64 and `mac` 64, every field valid hex (`malformed keystore` otherwise, exit code 2); after decryption, a keystore with an `address` must decrypt to that address. `~/` is the home directory; a relative path starts at the workspace root. The `password` is resolved like any variable, so keep it in a secret (a `{{secret:provider/path}}` reference or a variable listed in the environment's `secrets`); its value is redacted from every output. Without `password`, the password is asked on the terminal with no echo (a terminal that cannot be read is reported with its I/O error, exit code 2); without a terminal, the gate refuses:

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

4. No transaction field (`to`, `args`, `value`, `gas`, the fees, `nonce`) comes from a secret: the confirmation must show what is signed, so `evm-tx: 'args' comes from a secret; a transaction field must be shown in the confirmation, so it cannot hold a secret; nothing was signed or sent`.
5. A valid signer id configured in `[evm.signers]` (exit code 2 when missing, invalid or unknown).
6. The environment pins `chain_id`: `signing requires 'chain_id' in environment '<env>'` otherwise (or `signing requires an environment with 'chain_id'; select one with --env` without an environment).
7. The node's `eth_chainId`, asked again right before signing, equals the pin: `chain mismatch: environment 'dev' expects 31337 (Anvil/Hardhat local), endpoint reports 11155111 (Sepolia)` otherwise.
8. The transaction can be confirmed: a terminal, or `--yes-local` on a local chain (see [Confirmation](#confirmation)). Without one the gate refuses here, before the key is unlocked or anything simulated.
9. The signer unlocks.
10. The simulation, `eth_call` from the signer at `pending` and `eth_estimateGas`, succeeds. A revert is shown decoded and refused:

   ```text
   error: simulation reverted: InsufficientBalance(available=5, required=10); nothing was signed or sent (add --force-revert to sign anyway)
   ```

   With `--force-revert` the gate goes on, prints `warning: SIMULATION REVERTED: ...`, and the confirmation starts with the same warning. A reverting call has no gas estimate, so a forced transaction needs `gas` in the request.

11. The confirmation (see below).
12. `eth_chainId` once more, immediately before signing.

Nothing is signed or sent by a refusal, and loading a workspace with `evm-tx` files never calls a node or touches a signer. Ctrl+C at any prompt, or anywhere before the transaction is signed, stops with `interrupted; nothing was signed or sent` and exit code 130; the gate checks once more right after the confirmation and right before signing. Once the transaction is signed, Ctrl+C no longer drops it: the send is awaited, and Ctrl+C during the receipt wait ends it with the audit record (`"status": "pending"`, or `broadcast-unknown` when the send failed), `error: interrupted; transaction 0x… was sent and its receipt was not awaited; check the explorer for 0x…`, the explorer link, and exit code 3.

## Fees, gas and nonces

Values set in the request win; the rest are suggested by the node and labelled `(suggested)` in the confirmation. Fees take every quantity form (`30 gwei`, `0x6fc23ac00`, wei as a decimal integer).

- **EIP-1559** (the default, `type: eip1559`): `eth_feeHistory(10, "latest", [50])`. `max_priority_fee` is the median of the 50th-percentile rewards of those blocks; `max_fee` is 2 × the latest base fee + the priority fee. A node that reports no base fee is an error asking for `type: legacy`. `gas_price` is refused on an EIP-1559 transaction, and a `max_priority_fee` above `max_fee` is refused.
- **Legacy** (`type: legacy`): `gas_price`, by default `eth_gasPrice`; EIP-155 replay protection embeds the chain id. `max_fee` and `max_priority_fee` are refused.
- **Gas limit**: `gas`, by default the simulation's `eth_estimateGas` × 1.2, rounded up.
- **Ceilings**: a node cannot inflate what is signed. A suggested `max_fee`, `max_priority_fee` or `gas_price` above 10 000 gwei, or a gas limit from the estimate above 30 000 000, is refused (exit code 4) with a message asking to set the value in the request; values set in the request are not capped.
- **Max cost**: `--yes-local` never signs a transaction whose max cost (gas limit × max fee, plus the value) is above `[evm] max_cost`, a decimal amount of the chain's native unit (default `"1"`, 1 ETH): `max cost 300 ETH is above [evm] max_cost (1 ETH); --yes-local does not sign above it (confirm on a terminal, or raise [evm] max_cost); nothing was signed or sent` (exit code 4). On a terminal the summary starts with `WARNING: max cost … is above [evm] max_cost (…)` and the user decides.
- **Nonce**: `nonce`, by default `eth_getTransactionCount(signer, "pending")`. Within one process (one `rustman evm send`, or a whole `rustman run --sign`), later transactions of the same signer on the same chain count up from the last one sent instead of asking again; a transaction whose send failed counts too, since it may be out. A `nonce too low` or `replacement underpriced` answer is reported as the node sent it (exit code 3, see [unknown broadcast](#unknown-broadcast)) and nothing is retried.

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

The amount of an ERC-20 `transfer`, `approve` or `transferFrom` is shown in token units with the token's `decimals()` and `symbol()` when the token answers them, and the raw integer stays in parentheses (the decimals come from the node). `max cost` is the value plus the gas limit at the max fee (or gas price). Only `y` or `yes` goes on; anything else cancels with `cancelled; nothing was signed or sent` (exit code 4). Input typed before a question is discarded when it is asked, so an answer typed ahead never confirms a summary not yet shown (on Unix; on Windows the console's pending input is not discarded).

- **Chain 1 and `[evm] protected_chains`**: after `y`, the chain name must be typed exactly (`type the chain name (Ethereum Mainnet) to confirm:`). Anything else cancels with `the chain name did not match 'Ethereum Mainnet'; cancelled; nothing was signed or sent`. Chain 1 and the other built-in chains always use their built-in name; a protected chain configured in `[evm.chains.<id>]` without a name is never confirmed (`protected chain <id> has no name to type; set [evm.chains.<id>] name`).
- **No terminal** (CI, a pipe): signing is allowed only on chain 31337 and the chain ids of `[evm] headless_signing_chains` (test and private chains only: a built-in mainnet such as Base, OP Mainnet, Arbitrum One or Polygon PoS there is a configuration error), and only with `--yes-local`, which confirms without a prompt (`confirmed by --yes-local on Anvil/Hardhat local (31337)`). Elsewhere:

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
max_cost = "0.5"                     # --yes-local signs up to 0.5 of the native unit (default 1)
```

Every value of the summary, and the chain name in the typed-name question, is kept on one line: newlines, tabs, control, bidi and zero-width characters show as `\u{…}`. The summary is never passed through secret redaction (a short secret would hide part of an address); it holds no secret: no password, and no transaction field from a secret (refused at step 4).

## Sending and the receipt

The signed transaction is sent with `eth_sendRawTransaction`, the one place that method is allowed (every `jsonrpc` request, `evm-call`, agent call, and `kind: http` request whose body is a JSON-RPC call or batch is refused it, and every other sending method). The hash is printed (`sent 0x…`), then the receipt is awaited:

- `--confirmations N` (default 1, the block that includes it) blocks must be on top of the head;
- `--receipt-timeout` (default `120s`): when it passes, the command prints `error: transaction 0x… was sent but not mined within 120s` and the explorer link (when the chain has an explorer), and exits with code 3. The transaction may still be mined later: check it with `rustman evm tx <hash>`.
- A receipt poll that fails (the node is down, rate-limited, ...) is tried again, waiting twice as long after each failure in a row (up to 5 s), until the receipt timeout. When the last poll still failed, the command prints `error: transaction 0x… was sent but its receipt could not be read within 120s: <the node's error>` and the explorer link, and exits with code 3; the same happens when the mined transaction cannot be decoded. The audit record is written either way, with `"status": "receipt-unknown"`.

### Unknown broadcast

When `eth_sendRawTransaction` itself fails (the connection drops or times out, the node answers a 5xx or an RPC error such as `nonce too low`, or it answers something other than the transaction's hash), the transaction may have been broadcast all the same. The hash is computed locally from the signed bytes (keccak-256 of the EIP-2718 encoding), the failure is printed as a warning (`warning: eth_sendRawTransaction: …; transaction 0x… may have been sent; waiting for its receipt`; a mismatch names both hashes: `the node answered hash 0x…, expected 0x…`), and the receipt of the local hash is awaited as usual. If it comes, the transaction is checked like any other, with the failure kept as a warning. If it does not, the command prints `error: transaction 0x… may have been sent (<the failure>) but no receipt came within 120s; check the explorer for 0x… before sending it again`, the explorer link (when the chain has one), and exits with code 3; the audit record keeps the local hash with `"status": "broadcast-unknown"`. Check the hash (explorer or `rustman evm tx <hash>`) before signing the transaction again.

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

Every sent transaction is recorded in `request-history` (unless `--no-history` or `[history] enabled = false`), also when the receipt timed out (`"status": "pending"`), could not be read (`"status": "receipt-unknown"`), or the send itself failed (`"status": "broadcast-unknown"`, with the locally computed hash). The entry's `record.evm` holds:

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
| 2 | A configuration error: an unknown or invalid signer, a missing keystore, a keystore above the key-derivation limits or malformed, a mainnet in `[evm] headless_signing_chains`, an invalid `[evm] max_cost`, an unreadable password prompt, no keychain, a hardware-wallet signer. |
| 3 | The node could not be reached, or the transaction's send failed and no receipt came for its local hash (`broadcast-unknown`: it may have been sent), or the transaction was sent but its receipt did not come within `--receipt-timeout` or could not be read. |
| 4 | A gate refusal: signing disabled, key material as a signer, no `--sign`, no `chain_id` pin, a chain mismatch, no way to confirm (no terminal on a non-local chain), a wrong or missing keystore password, a transaction field from a secret, a max cost above `[evm] max_cost` under `--yes-local`, a reverting simulation, a declined confirmation or a wrong chain name. |
| 130 | Ctrl+C before the transaction was signed; nothing was signed or sent. Ctrl+C after it exits with code 3, its audit record kept. |

## In `rustman run`

`rustman run` skips `evm-tx` files unless `--sign` is passed; they are reported as `skipped (signing not requested)` and are not failures. With `--sign`, each `evm-tx` file of the run goes through the same gate as `rustman evm send`, one at a time and in run order:

```sh
rustman run . --env anvil --sign --yes-local
```

- The signer is the request's `signer`, or `RUSTMAN_EVM_SIGNER`.
- The preview, the summary and the receipt go to stderr; stdout is the run's report.
- `--yes-local` (which requires `--sign`) confirms without a prompt on chain 31337 and `[evm] headless_signing_chains` only. Without a terminal, every `evm-tx` file on another chain is refused, and the run exits with code 4. On a terminal, each transaction is confirmed as with `rustman evm send`.
- Nonces count up across the run: two transactions of one signer use the pending count, then that count + 1, without asking the node again.
- A refusal is the request's error and makes the run exit with code 4; a receipt timeout, an unreadable receipt or an unknown broadcast is an error (exit code 3), with the audit record kept; Ctrl+C before a transaction is signed skips it as interrupted (exit code 130), and Ctrl+C during its receipt wait keeps its audit record (`pending`) and makes the interrupted run exit with code 3; a failed receipt assertion or a transaction that reverted on chain fails the request (exit code 1). The receipt timeout is 120 s and one confirmation is awaited.

## In the TUI

An `evm-tx` file opens read-only in the EVM pane, with the notice ``evm-tx: Alt+T (evm.sign) signs and sends it after your confirmation; [evm] allow_signing = true is required``. `Ctrl+R` (`request.send`) never signs: the request is skipped as `signing not requested`, as in a plain `rustman run`.

`evm.sign` (`Alt+T`, or **Sign and send evm-tx (confirm first)** in the palette) is the TUI's counterpart of `--sign`. It takes the request through the same gate as `rustman evm send --sign`, with every precondition shared: `[evm] allow_signing = true`, a signer from `RUSTMAN_EVM_SIGNER` or the request's `signer` (keystore or keychain signers; raw keys refused), the environment's `chain_id` pin and the node's `eth_chainId`, the simulation, fees and nonces, then the receipt wait and the audit record. `evm.sign_force_revert` (palette only) is the `--force-revert` equivalent. There is no `--yes-local` equivalent: every transaction is confirmed in the dialog, on chain 31337 too.

The gate asks in a modal dialog:

- the keystore password (when the signer has no `password` secret), never echoed;
- the decoded summary, the same fields as on the command line, each value escaped and kept in its own column, with **Cancel** preselected. A forced transaction carries a `SIMULATION REVERTED: …` banner, and a max cost above `[evm] max_cost` a warning;
- on chain 1 and `[evm] protected_chains`, after **Sign and send**, the chain name, typed exactly.

`Esc` (or `Ctrl+C`) at any of them stops the gate: nothing is signed or sent and the request is shown as skipped (interrupted). Choosing **Cancel** declines (`cancelled; nothing was signed or sent`). A wrong chain name cancels.

The gate runs off the UI loop, so the screen stays responsive while the password is checked and the receipt is awaited. The Result tab shows what the gate showed (the preview, the summary, `sent 0x…`, the decoded receipt and the explorer link), then the audit record's status, hash, block, gas used and fee. Another send from that tab is refused while it runs. `Esc` in the pane cancels it: before the signature nothing is sent; during the receipt wait the audit record is kept with status `pending` and the hash is shown. Quitting does the same, and waits for the audit record to be written before Rustman exits. History keeps the same audit entry as `rustman evm send`, including `receipt-unknown` and `broadcast-unknown`.

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
