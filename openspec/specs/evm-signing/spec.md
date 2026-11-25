# evm-signing Specification

## Purpose
The signing gate and the `evm-tx` request kind: signers, confirmation, simulation before signing, nonce handling, CI restrictions and audit. It is the only capability allowed to send a signed transaction.

## Requirements

### Requirement: evm-tx request files
A `kind: evm-tx` request file SHALL contain `rpc`, `to` (address or ENS, required), `abi`, `function`, `args`, and optional `value`, `gas`, `max_fee`, `max_priority_fee`, `gas_price` (legacy), `nonce`, `type` (`eip1559` default, or `legacy`), `signer` (a signer id) and `assertions`/`captures` evaluated on the receipt. Loading the file SHALL never sign or send anything.

#### Scenario: Load without side effects
- **GIVEN** a valid `evm-tx` file
- **WHEN** the workspace loads or the file is opened in the TUI
- **THEN** no RPC call and no signer access SHALL occur

#### Scenario: Contract creation rejected
- **GIVEN** an `evm-tx` file without `to`
- **WHEN** it loads
- **THEN** loading SHALL fail with `contract deployment is out of scope; use Foundry or Hardhat`

### Requirement: Signing disabled by default
Signing SHALL require both `[evm] allow_signing = true` in `rustman.toml` and an explicit per-invocation opt-in: `--sign` on the CLI (`rustman evm send`, `rustman run`) or the `evm.sign` action of the TUI's `evm-tx` pane. If either is missing, the system SHALL refuse with exit code 4 and a message naming what is missing. `rustman run` SHALL skip `evm-tx` files unless `--sign` is passed.

#### Scenario: Workspace not opted in
- **GIVEN** `rustman.toml` has no `[evm] allow_signing`
- **WHEN** the user runs `rustman evm send transfer.rman.yaml --sign`
- **THEN** it SHALL be refused with `signing is disabled for this workspace; set [evm] allow_signing = true` and exit code 4

#### Scenario: Flag missing
- **GIVEN** `allow_signing = true`
- **WHEN** the user runs `rustman evm send transfer.rman.yaml` without `--sign`
- **THEN** it SHALL print the decoded transaction preview and refuse with `add --sign to sign and send` and exit code 4

#### Scenario: Runner skips evm-tx
- **WHEN** `rustman run .` includes an `evm-tx` file and `--sign` is absent
- **THEN** that file SHALL be reported as `skipped (signing not requested)` and SHALL NOT count as a failure

#### Scenario: TUI opens evm-tx read-only
- **WHEN** an `evm-tx` file is opened in the TUI, or sent with `request.send`
- **THEN** it SHALL be shown read-only with a notice naming `evm.sign`, and nothing SHALL be signed or sent without `evm.sign`

### Requirement: TUI signing dialog
`evm.sign` SHALL run the same gate as `rustman evm send --sign`, off the UI loop, and ask in a dialog: the keystore password without echo, the decoded summary (each value escaped, `Cancel` preselected, a `SIMULATION REVERTED` banner when forced) and the typed chain name where required. `Esc` SHALL stop with nothing signed or sent. The TUI SHALL never sign without the dialog's confirmation, and SHALL keep the audit entry when cancelled or quit after sending.

#### Scenario: Esc at the confirmation
- **GIVEN** `allow_signing = true` and the summary shown in the dialog
- **WHEN** the user presses `Esc`
- **THEN** nothing SHALL be signed or sent and the request SHALL be shown as interrupted

#### Scenario: Signed from the TUI
- **GIVEN** an Anvil environment pinning chain 31337
- **WHEN** the user runs `evm.sign`, enters the password and chooses `Sign and send`
- **THEN** the transaction SHALL be sent, its hash and decoded receipt shown, and history SHALL hold the same audit entry as the CLI

#### Scenario: Quit while pending
- **GIVEN** a transaction sent from the TUI whose receipt is awaited
- **WHEN** the user quits
- **THEN** the audit entry SHALL be recorded with status `pending` before the process exits

### Requirement: TUI confirmation integrity
The TUI confirmation SHALL keep its question, choices and hints on screen; a summary taller than the room left SHALL scroll, and `Sign and send` SHALL be accepted only once every summary row was drawn. Values SHALL be cut by display width under their own column. Keys other than `Esc` SHALL be ignored for 500 ms after the confirmation opens. Only one transaction SHALL be signed at a time across tabs.

#### Scenario: Summary taller than the terminal
- **GIVEN** a decoded call whose calldata wraps past the bottom of an 80x24 terminal
- **WHEN** the user selects `Sign and send` and presses `Enter` before scrolling
- **THEN** nothing SHALL be signed and the next page of the summary SHALL be shown

#### Scenario: Keys typed as the dialog opens
- **GIVEN** the user is pressing `Down` then `Enter` in another overlay
- **WHEN** the confirmation opens between the two keys
- **THEN** both keys SHALL be ignored and `Cancel` SHALL stay selected

#### Scenario: Second tab while signing
- **GIVEN** a transaction being signed in one tab
- **WHEN** the user runs `evm.sign` in another tab
- **THEN** it SHALL be refused and no second gate SHALL start

### Requirement: Approved signers only
Signers SHALL be configured in `[evm.signers.<id>]` as one of: `keystore` (path to a Web3 Secret Storage JSON file, password prompted or from a secret) or `keychain` (key stored in the OS keychain by `rustman evm signer import`). Raw private keys SHALL be rejected wherever they appear: request files, environments, variables, CLI flags or environment variables.

#### Scenario: Raw key in variable rejected
- **GIVEN** an environment variable `pk` whose value is 64 hex characters, used as a signer value
- **WHEN** the transaction is prepared
- **THEN** it SHALL be refused with `raw private keys are not accepted; import the key with 'rustman evm signer import'`

#### Scenario: Malformed keystore refused
- **GIVEN** a keystore whose `cipher` is not `aes-128-ctr`, whose IV is not 32 hex digits, or whose key does not match its `address`
- **WHEN** the signer is unlocked
- **THEN** it SHALL fail with a configuration error (exit code 2) and nothing SHALL be signed

#### Scenario: Keystore password from secret
- **GIVEN** `[evm.signers.deployer] keystore = "~/.foundry/keystores/deployer"` and `password = "{{secret:deployer_pw}}"`
- **WHEN** a transaction is signed
- **THEN** the keystore SHALL be decrypted in memory and the password SHALL never appear in output, history or logs

### Requirement: Hardware wallet signers deferred
Hardware-wallet signers (`ledger`/`trezor`) are out of scope for this change. A configured one SHALL be refused when used, with a configuration error (exit code 2) saying it is not available, and nothing SHALL be signed or sent.

#### Scenario: Hardware wallet not available
- **GIVEN** a `ledger` signer
- **WHEN** a transaction is to be signed with it
- **THEN** it SHALL fail with a configuration error (exit code 2) saying Ledger signing is not available, and nothing SHALL be signed or sent

### Requirement: Signer values never echoed
Signer ids SHALL be 1 to 64 of `[A-Za-z0-9_-]` and not hex-like. Likely key material (hex runs of 60 or more digits, seed phrases of 12 to 24 lowercase words) SHALL be refused as a raw key wherever a signer value can come from. A signer value that is not a configured id SHALL never be echoed in the preview, errors or reports.

#### Scenario: Near-key refused
- **GIVEN** `--signer` set to 63 hex digits, or to a 12-word seed phrase
- **WHEN** the user runs `rustman evm send transfer.rman.yaml --sign`
- **THEN** it SHALL be refused as a raw private key with exit code 4, without the value in any output

#### Scenario: Unknown signer not echoed
- **GIVEN** `--signer` set to a value that is not a configured signer id
- **WHEN** the user runs `rustman evm send transfer.rman.yaml --sign`
- **THEN** it SHALL fail with `unknown signer (configured: <ids>)` and exit code 2, and the value SHALL appear neither in the preview nor in the error

### Requirement: Mandatory decoded confirmation
Before signing, the system SHALL show a confirmation containing: chain name and id, signer address, `to` (with ENS name if any), decoded function and arguments, value in native units, gas limit, max fee and priority fee (or gas price), nonce, and the maximum total cost. The user SHALL confirm interactively. There SHALL be no flag that skips this confirmation on non-local chains.

#### Scenario: Confirmation content
- **WHEN** the user signs `transfer(to=bob.eth, amount=10 USDC)` on Sepolia
- **THEN** the confirmation SHALL list `Sepolia (11155111)`, the signer, `0x… (bob.eth)`, `transfer(to, amount=10.00 USDC)`, `value 0 ETH`, gas, fees, nonce and `max cost`

#### Scenario: Declined
- **WHEN** the user answers no
- **THEN** nothing SHALL be signed or sent and the exit code SHALL be 4

#### Scenario: Interrupted
- **WHEN** the user presses Ctrl+C at the confirmation, or at any point before the transaction is signed
- **THEN** nothing SHALL be signed or sent, the error SHALL be `interrupted; nothing was signed or sent` and the exit code SHALL be 130

### Requirement: Chain id must match
Signing SHALL require the environment to pin `chain_id`, and the endpoint's `eth_chainId` SHALL equal it immediately before signing. The signed transaction SHALL embed that chain id (EIP-155). Any mismatch or missing pin SHALL refuse with exit code 4.

#### Scenario: Missing pin
- **GIVEN** an environment without `chain_id`
- **WHEN** a transaction is prepared
- **THEN** it SHALL be refused with `signing requires 'chain_id' in environment '<env>'`

### Requirement: Simulate before signing
Before the confirmation, the system SHALL simulate the transaction with `eth_call` (from the signer, at `pending`) and `eth_estimateGas`. If the simulation reverts, the system SHALL show the decoded revert and refuse with exit code 4, unless `--force-revert` is given, in which case the confirmation SHALL carry a prominent `SIMULATION REVERTED` warning.

#### Scenario: Revert blocks signing
- **GIVEN** a transfer that would revert with `InsufficientBalance(5, 10)`
- **WHEN** the user runs `rustman evm send t.rman.yaml --sign`
- **THEN** the revert SHALL be shown and the command SHALL exit with code 4 without asking to sign

### Requirement: Mainnet typed confirmation
For chain id 1, and for any chain listed in `[evm] protected_chains`, the confirmation SHALL additionally require typing the chain name exactly (e.g. `Ethereum Mainnet`). A wrong entry SHALL cancel.

#### Scenario: Typed chain name
- **GIVEN** a transaction on chain 1
- **WHEN** the user types `mainnet` instead of `Ethereum Mainnet`
- **THEN** signing SHALL be cancelled with exit code 4

### Requirement: Headless signing restricted to local chains
Without an interactive terminal, signing SHALL be allowed only on chain id 31337, or on chain ids listed in `[evm] headless_signing_chains`, and only with `--sign` and `--yes-local`. On any other chain, non-interactive signing SHALL be refused with exit code 4, right after the chain id check and before any signer is unlocked or the transaction simulated.

#### Scenario: CI against Anvil
- **GIVEN** a CI job (no TTY) targeting an Anvil node with chain id 31337
- **WHEN** `rustman run . --sign --yes-local` executes `evm-tx` files
- **THEN** transactions SHALL be signed without an interactive prompt

#### Scenario: CI against Sepolia refused
- **GIVEN** no TTY and chain 11155111 not in `headless_signing_chains`
- **WHEN** `rustman run . --sign --yes-local` runs
- **THEN** each `evm-tx` file SHALL be refused with exit code 4 and no signer SHALL be unlocked

### Requirement: Agents can never sign
`agent-mcp-interface` SHALL NOT expose any tool that signs, sends a transaction, imports a key or changes signing configuration. Requests made through the agent interface SHALL never reach a signer, even if a request file of kind `evm-tx` is referenced.

#### Scenario: Agent sends saved evm-tx
- **WHEN** an agent calls `send_request` with the id of an `evm-tx` file
- **THEN** the call SHALL fail with `evm-tx requests cannot be sent through the agent interface`

### Requirement: Transaction types and fees
The system SHALL build EIP-1559 transactions by default, using `eth_feeHistory` to suggest `max_fee` (2 × base fee + priority) and `max_priority_fee` (median of the last 10 blocks' 50th percentile) unless set. `type: legacy` SHALL use `gas_price` (default from `eth_gasPrice`). The gas limit SHALL default to the estimate × 1.2.

#### Scenario: Default fee suggestion
- **GIVEN** no fee fields in the file
- **WHEN** the transaction is prepared on an EIP-1559 chain
- **THEN** the confirmation SHALL show suggested `max_fee` and `max_priority_fee` and label them `suggested`

### Requirement: Nonce management
Without an explicit `nonce`, the system SHALL use `eth_getTransactionCount(signer, "pending")`. Within one `rustman run`, consecutive transactions from the same signer SHALL use increasing nonces without re-querying. A `nonce too low` or `replacement underpriced` RPC error SHALL be reported verbatim and the transaction SHALL NOT be retried automatically.

#### Scenario: Sequential transactions in a run
- **GIVEN** two `evm-tx` files from the same signer in one run
- **WHEN** both are signed
- **THEN** the second SHALL use the first's nonce + 1

### Requirement: Receipt wait and audit
After sending, the system SHALL wait for the receipt (default timeout 120 s, `--confirmations N` default 1), evaluate receipt assertions and captures, and record an exchange in `request-history` with chain id, signer address, transaction hash, decoded call, status and fees. Signed raw bytes and key material SHALL never be stored. A timeout SHALL print the hash and explorer URL and exit with code 3.

#### Scenario: Audit record
- **WHEN** a transaction is mined successfully
- **THEN** history SHALL contain an entry with its hash, chain, signer, decoded call and gas used, and the CLI SHALL print the explorer link

#### Scenario: Receipt timeout
- **GIVEN** the transaction is not mined within 120 s
- **THEN** the command SHALL print the hash and explorer URL and exit with code 3

### Requirement: Unreadable receipt still audited
Failing receipt polls SHALL be retried with backoff within the receipt timeout. If the receipt still cannot be read, the system SHALL record the audit entry (hash, chain, signer, `to`, nonce; no signed bytes or key material) with status `receipt-unknown`, print the hash and explorer URL and exit with code 3.

#### Scenario: Receipt unreadable
- **GIVEN** the node accepts the transaction, then fails every `eth_getTransactionReceipt` poll
- **WHEN** the receipt timeout passes
- **THEN** history SHALL contain an audit entry with the hash, chain, signer, `to` and nonce and status `receipt-unknown`, and the command SHALL exit with code 3

### Requirement: Unknown broadcast still audited
If `eth_sendRawTransaction` fails (transport error, RPC error, or an answer other than the hash computed locally from the signed bytes), the system SHALL warn with the failure (naming both hashes on a mismatch), still wait for the receipt of the local hash, and, if none comes, record the audit entry with the local hash and status `broadcast-unknown`, say the transaction may have been sent, and exit with code 3.

#### Scenario: Broadcast outcome unknown
- **GIVEN** the node drops the connection, answers an RPC error, or answers another hash for `eth_sendRawTransaction`
- **WHEN** no receipt comes within the receipt timeout
- **THEN** history SHALL contain an audit entry with the local hash, chain, signer, `to` and nonce and status `broadcast-unknown`, without the signed bytes, and the command SHALL exit with code 3

### Requirement: Interrupted after sending still audited
Once a transaction is signed, Ctrl+C SHALL NOT drop it: the send SHALL be awaited, and Ctrl+C during the receipt wait SHALL record the audit entry with status `pending` (or `broadcast-unknown`), print the hash and explorer URL and exit with code 3, also in `rustman run --sign`.

#### Scenario: Ctrl+C during the receipt wait
- **GIVEN** the node accepted the transaction and does not mine it
- **WHEN** the user presses Ctrl+C after `sent 0x…`
- **THEN** history SHALL contain the audit entry with status `pending` and the command SHALL exit with code 3

### Requirement: Confirmation shows what is signed
The confirmation SHALL NOT pass through secret redaction. A transaction field resolved from a secret SHALL be refused with exit code 4. Input typed before a prompt SHALL be discarded (Unix).

#### Scenario: Field from a secret
- **GIVEN** an `evm-tx` argument resolved from a secret
- **WHEN** the user runs `rustman evm send t.rman.yaml --sign`
- **THEN** it SHALL be refused with exit code 4 before anything is signed

#### Scenario: Type-ahead discarded
- **GIVEN** `y` typed before the summary is shown
- **WHEN** the prompt is asked
- **THEN** that input SHALL be discarded and SHALL NOT confirm

### Requirement: Headless signing limits
A built-in mainnet in `[evm] headless_signing_chains` SHALL be a configuration error. `--yes-local` SHALL refuse (exit code 4) a max cost above `[evm] max_cost` (default 1 native unit); on a terminal the summary SHALL warn instead.

#### Scenario: Mainnet listed as headless
- **GIVEN** `headless_signing_chains = [8453]`
- **WHEN** a transaction is prepared
- **THEN** it SHALL fail with a configuration error naming Base as a mainnet

#### Scenario: Max cost above the cap
- **GIVEN** a max cost of 300 ETH on chain 31337 and the default `max_cost`
- **WHEN** `--yes-local` confirms it
- **THEN** it SHALL be refused with exit code 4 and nothing SHALL be signed
