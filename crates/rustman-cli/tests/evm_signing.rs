//! `rustman evm send` and `rustman evm signer import` (add-evm-toolkit
//! tasks 5.1, 5.2): every gate refusal exits with code 4 and its exact
//! message, a raw private key is refused from every place a signer value
//! can come from, keystore and keychain signers unlock, and no password or
//! key ever reaches the output. Stub nodes on loopback; Anvil's well-known
//! dev accounts; no test touches the internet.

mod common;

use std::path::{Path, PathBuf};

use common::{rustman, write};
use rustman_test_support::{JsonrpcHttpServer, RpcReply};
use serde_json::{Value, json};

const TOKEN: &str = "0x5FbDB2315678afecb367f032d93F642f64180aa3";
const BOB: &str = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
const ANVIL0: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
const ANVIL1_KEY: &str = "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";
const RAW_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const PASSWORD: &str = "rustman-test-password";
const RAW_KEY_REFUSED: &str =
    "error: raw private keys are not accepted; import the key with 'rustman evm signer import'";
/// Every precondition passed and the transaction was signed: the stub
/// node refuses it, and nothing is retried.
const NODE_REFUSED: &str = "eth_sendRawTransaction: rpc error -32000: nonce too low";
/// Without a terminal, a local chain needs `--yes-local`.
const NO_TERMINAL: &str = "error: no terminal to confirm on; add --yes-local to sign on Anvil/Hardhat local (31337) without a prompt";

/// How the stub node answers `eth_call` and `eth_estimateGas`.
#[derive(Clone, Copy)]
enum Simulation {
    Succeeds,
    Reverts,
    /// Succeeds, `eth_estimateGas` answering after this many milliseconds.
    Slow(u64),
}

/// How the stub node answers `eth_sendRawTransaction` and the receipt
/// polls.
#[derive(Clone, Copy)]
enum Broadcast {
    /// `nonce too low`, and no receipt ever comes.
    Refuses,
    /// `nonce too low`, yet the transaction is mined (its receipt comes,
    /// but the transaction cannot be fetched).
    RefusesButMined,
    /// The connection is closed without an answer; no receipt comes.
    Drops,
    /// Another hash is answered; no receipt comes.
    AnswersOtherHash,
    /// The transaction is accepted (its hash answered), then every
    /// `eth_getTransactionReceipt` fails.
    AcceptsThenReceiptFails,
    /// The transaction is accepted and never mined (`--no-mining`).
    Accepts,
}

/// The hash the `AnswersOtherHash` node answers.
const OTHER_HASH: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";

struct Node {
    _runtime: tokio::runtime::Runtime,
    server: JsonrpcHttpServer,
}

impl Node {
    fn start(chain_id: &'static str, simulation: Simulation) -> Self {
        Self::start_with(chain_id, simulation, Broadcast::Refuses)
    }

    fn start_with(chain_id: &'static str, simulation: Simulation, broadcast: Broadcast) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let delay = match simulation {
            Simulation::Slow(ms) => std::time::Duration::from_millis(ms),
            _ => std::time::Duration::ZERO,
        };
        let simulation = match simulation {
            Simulation::Slow(_) => Simulation::Succeeds,
            other => other,
        };
        let server = runtime.block_on(JsonrpcHttpServer::start(move |req, _| {
            let body = req.json();
            let method = body["method"].as_str().unwrap_or_default();
            let answered = match (method, broadcast) {
                ("eth_sendRawTransaction", Broadcast::Drops) => return RpcReply::dropped(),
                ("eth_sendRawTransaction", Broadcast::AnswersOtherHash) => Ok(json!(OTHER_HASH)),
                (
                    "eth_sendRawTransaction",
                    Broadcast::AcceptsThenReceiptFails | Broadcast::Accepts,
                ) => Ok(json!(raw_hash(&body["params"][0]))),
                ("eth_getTransactionReceipt", Broadcast::AcceptsThenReceiptFails) => {
                    Err(json!({"code": -32000, "message": "upstream unavailable"}))
                }
                ("eth_getTransactionReceipt", Broadcast::RefusesButMined) => {
                    Ok(json!({"blockNumber": "0x1", "status": "0x1"}))
                }
                ("eth_call", _) if matches!(simulation, Simulation::Succeeds) => {
                    token_answer(&body["params"][0]["data"])
                        .map_or_else(|| answer(chain_id, simulation, method), Ok)
                }
                _ => answer(chain_id, simulation, method),
            };
            let reply = match answered {
                Ok(result) => json!({"jsonrpc": "2.0", "id": body["id"], "result": result}),
                Err(error) => json!({"jsonrpc": "2.0", "id": body["id"], "error": error}),
            };
            match method {
                "eth_estimateGas" => RpcReply::json(&reply).delayed(delay),
                _ => RpcReply::json(&reply),
            }
        }));
        Self {
            _runtime: runtime,
            server,
        }
    }

    fn url(&self) -> String {
        self.server.url("/")
    }

    fn calls(&self) -> Vec<Value> {
        self.server.hits().iter().map(|h| h.json()).collect()
    }
}

/// The token's answers to `decimals()` (6) and `symbol()` (`USDC`).
fn token_answer(data: &Value) -> Option<Value> {
    match data.as_str()? {
        "0x313ce567" => Some(json!(format!("0x{:0>64}", "6"))),
        "0x95d89b41" => Some(json!(format!(
            "0x{:0>64}{:0>64}{:0<64}",
            "20", "4", "55534443"
        ))),
        _ => None,
    }
}

/// The hash of the raw transaction `raw` (`0x` hex), as a node answers it.
fn raw_hash(raw: &Value) -> String {
    let bytes = alloy_primitives::hex::decode(raw.as_str().unwrap()).unwrap();
    alloy_primitives::keccak256(bytes).to_string()
}

fn answer(chain_id: &str, simulation: Simulation, method: &str) -> Result<Value, Value> {
    // `Error(string)` with "transfer amount exceeds balance".
    let revert = "0x08c379a00000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000001f7472616e7366657220616d6f756e7420657863656564732062616c616e636500";
    match (method, simulation) {
        ("eth_chainId", _) => Ok(json!(chain_id)),
        ("web3_clientVersion", _) => Ok(json!("anvil/v1.0.0")),
        ("eth_feeHistory", _) => Ok(json!({
            "oldestBlock": "0x1",
            "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
            "gasUsedRatio": [0.5],
            "reward": [["0x3b9aca00"]],
        })),
        ("eth_getTransactionCount", _) => Ok(json!("0x4")),
        ("eth_sendRawTransaction", _) => Err(json!({"code": -32000, "message": "nonce too low"})),
        ("eth_getTransactionReceipt", _) => Ok(Value::Null),
        ("eth_call", Simulation::Succeeds) => Ok(json!(format!("0x{:0>64}", "1"))),
        ("eth_estimateGas", Simulation::Succeeds) => Ok(json!("0xb411")),
        (_, Simulation::Reverts) => {
            Err(json!({"code": 3, "message": "execution reverted", "data": revert}))
        }
        _ => Err(json!({"code": -32601, "message": "method not found"})),
    }
}

fn output(cmd: &mut assert_cmd::Command) -> (Option<i32>, String, String) {
    let out = cmd.output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn keystore_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rustman-evm-signing/tests/fixtures/keystores/anvil0.json")
}

const SIGNERS: &str = "[evm.signers.deployer]\nkeystore = \"keys/anvil0.json\"\npassword = \"{{deployer_pw}}\"\n\
                       [evm.signers.ci]\nkeychain = true\n";

/// A workspace whose `dev` environment points `rpc` at `url`, with
/// `evm` added to `rustman.toml` and `env` to the environment, and a
/// `transfer` evm-tx signed by `deployer`.
fn workspace(url: &str, evm: &str, env: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        &format!("[workspace]\nname = \"t\"\nversion = 1\n\n{evm}"),
    );
    write(
        dir.path(),
        "environments/dev.yaml",
        &format!("{env}variables:\n  rpc: {url}\n"),
    );
    write(dir.path(), ".gitignore", ".rustman/\nkeys/\n");
    write(dir.path(), "transfer.rman.yaml", &transfer("deployer"));
    let keystore = std::fs::read_to_string(keystore_fixture()).unwrap();
    write(dir.path(), "keys/anvil0.json", &keystore);
    dir
}

fn transfer(signer: &str) -> String {
    format!(
        "version: 1\nkind: evm-tx\nname: transfer\nrpc: \"{{{{rpc}}}}\"\nto: \"{TOKEN}\"\nabi: erc20\nfunction: transfer\nargs: [\"{BOB}\", \"10\"]\nsigner: \"{signer}\"\n"
    )
}

fn enabled() -> String {
    format!("[evm]\nallow_signing = true\n\n{SIGNERS}")
}

const PINNED: &str = "chain_id: 31337\nsecrets: [deployer_pw]\n";

/// Store the keystore password as the `deployer_pw` secret of `dev`.
fn set_password(dir: &Path) {
    rustman(dir)
        .args(["secret", "set", "deployer_pw", "--env", "dev"])
        .write_stdin(PASSWORD)
        .assert()
        .success();
}

/// The node refused the signed transaction: it may have been sent all the
/// same, so the receipt was awaited (1 s) and the broadcast is unknown.
fn assert_refused_send(stderr: &str) {
    assert!(
        stderr.contains(&format!(
            "may have been sent ({NODE_REFUSED}) but no receipt came within 1s"
        )),
        "{stderr}"
    );
}

fn send(dir: &Path, extra: &[&str]) -> (Option<i32>, String, String) {
    let mut args = vec!["evm", "send", "transfer", "--env", "dev"];
    args.extend(extra);
    output(rustman(dir).args(args))
}

#[test]
fn a_workspace_without_allow_signing_is_refused() {
    // Scenario "Workspace not opted in".
    let node = Node::start("0x7a69", Simulation::Succeeds);
    let ws = workspace(&node.url(), SIGNERS, PINNED);
    let (code, stdout, stderr) = send(ws.path(), &["--sign"]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert_eq!(
        stderr.trim(),
        "error: signing is disabled for this workspace; set [evm] allow_signing = true"
    );
    assert!(node.calls().is_empty(), "nothing is sent");
}

#[test]
fn without_sign_the_preview_is_printed_and_refused() {
    // Scenario "Flag missing".
    let node = Node::start("0x7a69", Simulation::Succeeds);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    let (code, stdout, stderr) = send(ws.path(), &[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert_eq!(stderr.trim(), "error: add --sign to sign and send");
    assert_eq!(
        stdout,
        format!(
            "evm-tx transfer\n  to      {TOKEN}\n  call    transfer({BOB}, 10)\n  value   0\n  signer  deployer\n"
        )
    );
    assert!(node.calls().is_empty(), "nothing is sent");
}

#[test]
fn an_environment_without_chain_id_is_refused() {
    // Scenario "Missing pin".
    let node = Node::start("0x7a69", Simulation::Succeeds);
    let ws = workspace(&node.url(), &enabled(), "");
    let (code, _, stderr) = send(ws.path(), &["--sign"]);
    assert_eq!(code, Some(4), "{stderr}");
    assert_eq!(
        stderr.trim(),
        "error: signing requires 'chain_id' in environment 'dev'"
    );
    assert!(node.calls().is_empty(), "nothing is sent");
}

#[test]
fn a_chain_mismatch_is_refused() {
    let node = Node::start("0xaa36a7", Simulation::Succeeds);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    let (code, _, stderr) = send(ws.path(), &["--sign"]);
    assert_eq!(code, Some(4), "{stderr}");
    assert_eq!(
        stderr.trim(),
        "error: chain mismatch: environment 'dev' expects 31337 (Anvil/Hardhat local), endpoint reports 11155111 (Sepolia)"
    );
    let methods: Vec<_> = node.calls().iter().map(|c| c["method"].clone()).collect();
    assert_eq!(methods, ["eth_chainId"]);
}

#[test]
fn a_reverting_simulation_is_refused_unless_forced() {
    // Scenario "Revert blocks signing".
    let node = Node::start("0x7a69", Simulation::Reverts);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    set_password(ws.path());
    let (code, _, stderr) = send(ws.path(), &["--sign", "--yes-local"]);
    assert_eq!(code, Some(4), "{stderr}");
    assert_eq!(
        stderr.trim(),
        "error: simulation reverted: transfer amount exceeds balance; nothing was signed or sent (add --force-revert to sign anyway)"
    );
    // Forced: the revert leaves no gas estimate, so `gas` must be set.
    let forced = ["--sign", "--yes-local", "--force-revert"];
    let (code, _, stderr) = send(ws.path(), &forced);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.ends_with(
            "error: no gas estimate (the simulation reverted); set 'gas' in the request\n"
        ),
        "{stderr}"
    );
    let with_gas = format!("{}gas: 100000\n", transfer("deployer"));
    write(ws.path(), "transfer.rman.yaml", &with_gas);
    let forced = [&forced[..], &["--receipt-timeout", "1s"]].concat();
    let (code, stdout, stderr) = send(ws.path(), &forced);
    assert_eq!(code, Some(3), "{stderr}");
    assert!(
        stderr.contains("warning: SIMULATION REVERTED: reverted: transfer amount exceeds balance"),
        "{stderr}"
    );
    assert!(
        stdout.contains("WARNING: SIMULATION REVERTED: reverted: transfer amount exceeds balance"),
        "{stdout}"
    );
    assert_refused_send(&stderr);
}

#[test]
fn a_keystore_signer_passes_the_gate_and_its_password_never_shows() {
    // Scenario "Keystore password from secret".
    let node = Node::start("0x7a69", Simulation::Succeeds);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    set_password(ws.path());
    let (code, stdout, stderr) = send(
        ws.path(),
        &["--sign", "--yes-local", "--receipt-timeout", "1s"],
    );
    assert_eq!(code, Some(3), "{stdout}{stderr}");
    assert_refused_send(&stderr);
    let calls = node.calls();
    let simulated = calls.iter().find(|c| c["method"] == "eth_call").unwrap();
    assert_eq!(simulated["params"][0]["from"], ANVIL0, "from the signer");
    assert_eq!(simulated["params"][1], "pending");
    assert!(calls.iter().any(|c| c["method"] == "eth_estimateGas"));
    let sent = calls
        .iter()
        .filter(|c| c["method"] == "eth_sendRawTransaction");
    assert_eq!(sent.count(), 1, "a refused transaction is not retried");
    // The confirmation, with suggested fees, the nonce and the token
    // amount in token units (scenario "Confirmation content").
    for field in [
        "chain        Anvil/Hardhat local (31337)",
        &format!("signer       {ANVIL0} (deployer)"),
        &format!("to           {TOKEN}"),
        &format!("call         transfer(to={BOB}, amount=0.00001 USDC (10))"),
        "value        0 ETH",
        "gas          55317",
        "max fee      3 gwei (suggested)",
        "priority fee 1 gwei (suggested)",
        "nonce        4",
        "max cost     0.000165951 ETH",
    ] {
        assert!(stdout.contains(field), "{field}: {stdout}");
    }
    assert!(!format!("{stdout}{stderr}").contains(PASSWORD));
    assert_no_secret_material(ws.path());
}

#[test]
fn without_a_terminal_nothing_is_unlocked_for_a_send_that_cannot_be_confirmed() {
    // Regression: the non-TTY refusal came after the keystore was
    // decrypted and the transaction simulated.
    let node = Node::start("0x7a69", Simulation::Succeeds);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    set_password(ws.path());
    let (code, stdout, stderr) = send(ws.path(), &["--sign"]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert_eq!(stderr.trim(), NO_TERMINAL);
    let methods: Vec<_> = node.calls().iter().map(|c| c["method"].clone()).collect();
    assert_eq!(methods, ["eth_chainId"], "no simulation, no unlock");
    // A wrong password shows the key was never decrypted.
    rustman(ws.path())
        .args(["secret", "set", "deployer_pw", "--env", "dev"])
        .write_stdin("wrong")
        .assert()
        .success();
    let (code, _, stderr) = send(ws.path(), &["--sign"]);
    assert_eq!(code, Some(4), "{stderr}");
    assert_eq!(stderr.trim(), NO_TERMINAL);
}

/// Neither the keystore password nor a raw key is in any file Rustman
/// wrote under `.rustman/` (history, state, logs), apart from the secret
/// store itself.
fn assert_no_secret_material(root: &Path) {
    let mut stack = vec![root.join(".rustman")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            // The file secret store the tests use (RUSTMAN_SECRET_STORE=file)
            // holds the password on purpose.
            if path.file_name().is_some_and(|n| n == "secrets.yaml") {
                continue;
            }
            let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
            for secret in [PASSWORD, &RAW_KEY[2..], &ANVIL1_KEY[2..]] {
                assert!(
                    !text.contains(secret),
                    "{} holds secret material",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn a_raw_key_is_refused_from_every_location() {
    let node = Node::start("0x7a69", Simulation::Succeeds);
    let refused = |ws: &tempfile::TempDir, cmd: &mut assert_cmd::Command, place: &str| {
        let (code, stdout, stderr) = output(cmd);
        assert_eq!(code, Some(4), "{place}: {stdout}{stderr}");
        assert!(
            stderr.trim().ends_with(&RAW_KEY_REFUSED[7..]),
            "{place}: {stderr}"
        );
        assert!(
            !format!("{stdout}{stderr}").contains(&RAW_KEY[2..]),
            "{place}"
        );
        assert_no_secret_material(ws.path());
    };
    let args = ["evm", "send", "transfer", "--env", "dev", "--sign"];

    // The request file.
    let ws = workspace(&node.url(), &enabled(), PINNED);
    write(ws.path(), "transfer.rman.yaml", &transfer(RAW_KEY));
    refused(&ws, rustman(ws.path()).args(args), "request file");

    // An environment variable used as the signer (scenario "Raw key in
    // variable rejected").
    let ws = workspace(&node.url(), &enabled(), PINNED);
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!(
            "chain_id: 31337\nvariables:\n  rpc: {}\n  pk: \"{RAW_KEY}\"\n",
            node.url()
        ),
    );
    write(ws.path(), "transfer.rman.yaml", &transfer("{{pk}}"));
    refused(&ws, rustman(ws.path()).args(args), "environment");

    // A variable from the command line.
    let ws = workspace(&node.url(), &enabled(), PINNED);
    write(ws.path(), "transfer.rman.yaml", &transfer("{{pk}}"));
    let var = format!("pk={RAW_KEY}");
    refused(
        &ws,
        rustman(ws.path()).args(args).args(["--var", &var]),
        "--var",
    );

    // The --signer flag, and the environment variable behind it.
    let ws = workspace(&node.url(), &enabled(), PINNED);
    refused(
        &ws,
        rustman(ws.path()).args(args).args(["--signer", RAW_KEY]),
        "--signer",
    );
    refused(
        &ws,
        rustman(ws.path())
            .args(args)
            .env("RUSTMAN_EVM_SIGNER", RAW_KEY),
        "RUSTMAN_EVM_SIGNER",
    );

    // rustman.toml.
    let toml = format!(
        "[evm]\nallow_signing = true\n[evm.signers.deployer]\nprivate_key = \"{RAW_KEY}\"\n"
    );
    let ws = workspace(&node.url(), &toml, PINNED);
    refused(&ws, rustman(ws.path()).args(args), "rustman.toml");

    // `rustman evm signer import` takes an id, never a key.
    let ws = workspace(&node.url(), &enabled(), PINNED);
    let keychain = tempfile::tempdir().unwrap();
    refused(
        &ws,
        rustman(ws.path())
            .args(["evm", "signer", "import", RAW_KEY])
            .env("RUSTMAN_RECORD_TEST_KEYCHAIN", keychain.path())
            .write_stdin(ANVIL1_KEY),
        "signer import id",
    );
    assert!(node.calls().is_empty(), "nothing is sent");
}

#[test]
fn an_imported_keychain_signer_passes_the_gate() {
    let node = Node::start("0x7a69", Simulation::Succeeds);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    let keychain = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = output(
        rustman(ws.path())
            .args(["evm", "signer", "import", "ci"])
            .env("RUSTMAN_RECORD_TEST_KEYCHAIN", keychain.path())
            .write_stdin(format!("{ANVIL1_KEY}\n")),
    );
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert_eq!(
        stdout,
        format!(
            "imported signer 'ci' ({BOB}) into the OS keychain\nuse it from rustman.toml:\n  [evm.signers.ci]\n  keychain = true\n"
        )
    );
    assert!(!format!("{stdout}{stderr}").contains(&ANVIL1_KEY[2..]));
    let (code, _, stderr) = output(
        rustman(ws.path())
            .args([
                "evm",
                "send",
                "transfer",
                "--env",
                "dev",
                "--sign",
                "--yes-local",
                "--signer",
                "ci",
                "--receipt-timeout",
                "1s",
            ])
            .env("RUSTMAN_RECORD_TEST_KEYCHAIN", keychain.path()),
    );
    assert_eq!(code, Some(3), "{stderr}");
    assert_refused_send(&stderr);
    let calls = node.calls();
    let simulated = calls.iter().find(|c| c["method"] == "eth_call").unwrap();
    assert_eq!(simulated["params"][0]["from"], BOB);
    assert_no_secret_material(ws.path());
}

#[test]
fn signer_keys_never_fall_back_to_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let (code, _, stderr) = output(
        rustman(dir.path())
            .args(["evm", "signer", "import", "ci"])
            .write_stdin(ANVIL1_KEY),
    );
    assert_eq!(code, Some(2), "{stderr}");
    assert_eq!(
        stderr.trim(),
        "error: no OS keychain is available; signer keys are never stored in files"
    );
}

#[test]
fn unknown_signers_and_hardware_wallets_fail_clearly() {
    let node = Node::start("0x7a69", Simulation::Succeeds);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    let (code, _, stderr) = send(ws.path(), &["--sign", "--signer", "nobody"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert_eq!(
        stderr.trim(),
        "error: unknown signer (configured: ci, deployer)"
    );
    let toml = "[evm]\nallow_signing = true\n[evm.signers.cold]\nledger = \"m/44'/60'/0'/0/0\"\n";
    let ws = workspace(&node.url(), toml, PINNED);
    let (code, _, stderr) = send(ws.path(), &["--sign", "--yes-local", "--signer", "cold"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("signer 'cold': Ledger signing is not available yet"),
        "{stderr}"
    );
}

#[test]
fn without_a_terminal_a_non_local_chain_is_refused_even_with_yes_local() {
    // Scenario "CI against Sepolia refused".
    let node = Node::start("0xaa36a7", Simulation::Succeeds);
    let env = "chain_id: 11155111\nsecrets: [deployer_pw]\n";
    let ws = workspace(&node.url(), &enabled(), env);
    set_password(ws.path());
    let (code, stdout, stderr) = send(ws.path(), &["--sign", "--yes-local"]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert_eq!(
        stderr.trim(),
        "error: signing on Sepolia (11155111) needs an interactive confirmation; without a terminal only chain 31337 and [evm] headless_signing_chains can be signed on, with --yes-local"
    );
    // Refused right after the chain check: no key unlocked, no
    // simulation, no summary.
    let methods: Vec<_> = node.calls().iter().map(|c| c["method"].clone()).collect();
    assert_eq!(methods, ["eth_chainId"]);
    assert!(!stdout.contains("max cost"), "{stdout}");
    assert_no_secret_material(ws.path());
}

/// `rustman` on a terminal, from `dir`: what it printed and its exit code
/// once it ends, after answering each `(prompt, reply)` in turn.
#[cfg(unix)]
fn on_a_terminal(dir: &Path, args: &[&str], replies: &[(&str, &str)]) -> (u32, String) {
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_rustman"));
    cmd.args(args);
    cmd.cwd(dir);
    cmd.env("RUSTMAN_SECRET_STORE", "file");
    cmd.env("RUSTMAN_DATA_DIR", dir.join(".data"));
    cmd.env_remove("RUSTMAN_ENV");
    let size = PtySize {
        rows: 50,
        cols: 200,
        pixel_width: 0,
        pixel_height: 0,
    };
    let pty = native_pty_system().openpty(size).unwrap();
    let mut child = pty.slave.spawn_command(cmd).unwrap();
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().unwrap();
    let mut writer = pty.master.take_writer().unwrap();
    let seen = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&seen);
    let reading = std::thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });
    let mut from = 0;
    for (prompt, reply) in replies {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let text = seen.lock().unwrap().clone();
            if let Some(at) = text[from..].find(prompt) {
                from += at + prompt.len();
                break;
            }
            assert!(Instant::now() < deadline, "no prompt '{prompt}' in {text}");
            std::thread::sleep(Duration::from_millis(50));
        }
        writer.write_all(format!("{reply}\r").as_bytes()).unwrap();
        writer.flush().unwrap();
    }
    let status = child.wait().unwrap();
    drop(writer);
    drop(pty.master);
    let _ = reading.join();
    let text = seen.lock().unwrap().clone();
    (status.exit_code(), text)
}

#[cfg(unix)]
#[test]
fn answering_no_on_a_terminal_cancels() {
    // Scenario "Declined".
    let node = Node::start("0x7a69", Simulation::Succeeds);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    set_password(ws.path());
    let args = ["evm", "send", "transfer", "--env", "dev", "--sign"];
    let (code, seen) = on_a_terminal(
        ws.path(),
        &args,
        &[("sign and send this transaction? [y/N] ", "n")],
    );
    assert_eq!(code, 4, "{seen}");
    assert!(
        seen.contains("error: cancelled; nothing was signed or sent"),
        "{seen}"
    );
    assert!(seen.contains("max cost"), "{seen}");
    assert!(!seen.contains("type the chain name"), "{seen}");
    let methods: Vec<_> = node.calls().iter().map(|c| c["method"].clone()).collect();
    assert!(
        !methods.contains(&json!("eth_sendRawTransaction")),
        "{methods:?}"
    );
    assert!(!seen.contains(PASSWORD), "{seen}");
}

#[cfg(unix)]
#[test]
fn mainnet_needs_its_name_typed_exactly() {
    // Scenario "Typed chain name".
    let node = Node::start("0x1", Simulation::Succeeds);
    let env = "chain_id: 1\nsecrets: [deployer_pw]\n";
    let ws = workspace(&node.url(), &enabled(), env);
    set_password(ws.path());
    let args = ["evm", "send", "transfer", "--env", "dev", "--sign"];
    let (code, seen) = on_a_terminal(
        ws.path(),
        &args,
        &[
            ("sign and send this transaction? [y/N] ", "y"),
            (
                "type the chain name (Ethereum Mainnet) to confirm: ",
                "mainnet",
            ),
        ],
    );
    assert_eq!(code, 4, "{seen}");
    assert!(seen.contains("chain        Ethereum Mainnet (1)"), "{seen}");
    assert!(
        seen.contains(
            "error: the chain name did not match 'Ethereum Mainnet'; cancelled; nothing was signed or sent"
        ),
        "{seen}"
    );
    let methods: Vec<_> = node.calls().iter().map(|c| c["method"].clone()).collect();
    assert!(
        !methods.contains(&json!("eth_sendRawTransaction")),
        "{methods:?}"
    );
}

#[cfg(unix)]
#[test]
fn yes_local_never_skips_the_prompt_of_another_chain() {
    let node = Node::start("0xaa36a7", Simulation::Succeeds);
    let env = "chain_id: 11155111\nsecrets: [deployer_pw]\n";
    let ws = workspace(&node.url(), &enabled(), env);
    set_password(ws.path());
    let args = [
        "evm",
        "send",
        "transfer",
        "--env",
        "dev",
        "--sign",
        "--yes-local",
    ];
    let (code, seen) = on_a_terminal(
        ws.path(),
        &args,
        &[("sign and send this transaction? [y/N] ", "")],
    );
    assert_eq!(code, 4, "{seen}");
    assert!(
        seen.contains(
            "note: --yes-local applies only to chain 31337 and [evm] headless_signing_chains"
        ),
        "{seen}"
    );
    assert!(seen.contains("error: cancelled"), "{seen}");
}

#[test]
fn without_sign_the_preview_comes_before_any_signer_lookup() {
    // Scenario "Flag missing". Regression: an unknown or missing signer
    // was a configuration error (exit 2) before --sign was checked.
    let node = Node::start("0x7a69", Simulation::Succeeds);
    let ws = workspace(&node.url(), "[evm]\nallow_signing = true\n", PINNED);
    write(ws.path(), "transfer.rman.yaml", &transfer("nobody"));
    let (code, stdout, stderr) = send(ws.path(), &[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert_eq!(stderr.trim(), "error: add --sign to sign and send");
    assert!(
        stdout.contains("  signer  (not a configured signer)"),
        "{stdout}"
    );
    assert!(node.calls().is_empty(), "nothing is sent");
}

fn run_signing(dir: &Path, extra: &[&str]) -> (Option<i32>, String, String) {
    let mut args = vec!["run", ".", "--env", "dev"];
    args.extend(extra);
    output(rustman(dir).args(args))
}

#[test]
fn a_run_on_a_non_local_chain_without_a_terminal_is_refused() {
    // Scenario "CI against Sepolia refused", through `rustman run`.
    let node = Node::start("0xaa36a7", Simulation::Succeeds);
    let env = "chain_id: 11155111\nsecrets: [deployer_pw]\n";
    let ws = workspace(&node.url(), &enabled(), env);
    set_password(ws.path());
    let (code, stdout, stderr) = run_signing(ws.path(), &["--sign", "--yes-local"]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(
        stdout.contains("needs an interactive confirmation"),
        "{stdout}{stderr}"
    );
    // The preview goes to stderr (stdout is the report); refused before
    // the key is unlocked, so there is no summary.
    assert!(stderr.contains("evm-tx transfer"), "{stderr}");
    assert!(!stderr.contains("max cost"), "{stderr}");
    assert!(!format!("{stdout}{stderr}").contains(PASSWORD));
    let methods: Vec<_> = node.calls().iter().map(|c| c["method"].clone()).collect();
    assert_eq!(methods, ["eth_chainId"]);
    assert_no_secret_material(ws.path());
}

#[test]
fn a_run_with_sign_still_needs_the_workspace_opt_in() {
    let node = Node::start("0x7a69", Simulation::Succeeds);
    let ws = workspace(&node.url(), SIGNERS, PINNED);
    let (code, stdout, stderr) = run_signing(ws.path(), &["--sign", "--yes-local"]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(
        stdout.contains("signing is disabled for this workspace; set [evm] allow_signing = true"),
        "{stdout}"
    );
    assert!(node.calls().is_empty(), "nothing is sent");
}

#[test]
fn yes_local_in_a_run_needs_sign() {
    let node = Node::start("0x7a69", Simulation::Succeeds);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    let (code, _, stderr) = run_signing(ws.path(), &["--yes-local"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("--sign"), "{stderr}");
    let (code, stdout, _) = run_signing(ws.path(), &[]);
    assert_eq!(code, Some(0), "{stdout}");
    assert!(
        stdout.contains("skipped (signing not requested)"),
        "{stdout}"
    );
    assert!(node.calls().is_empty(), "nothing is sent");
}

/// Values that look like key material without being exactly a key: a
/// digit missing, padding, a seed phrase.
fn near_keys() -> [String; 3] {
    [
        RAW_KEY[..65].to_owned(),
        format!("0x00{}", &RAW_KEY[2..]),
        "test test test test test test test test test test test junk".to_owned(),
    ]
}

/// The part of `value` that must never be shown.
fn secret_part(value: &str) -> &str {
    value.trim_start_matches("0x").get(..40).unwrap_or(value)
}

#[test]
fn near_keys_and_unknown_signers_are_never_echoed() {
    // Regression: a value that was not exactly 64 hex digits, given as
    // the signer, was printed in the preview (`signer <value>`) and in
    // `unknown signer '<value>'`.
    let node = Node::start("0x7a69", Simulation::Succeeds);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    let args = ["evm", "send", "transfer", "--env", "dev", "--sign"];
    for value in near_keys() {
        for (place, cmd) in [
            (
                "--signer",
                rustman(ws.path()).args(args).args(["--signer", &value]),
            ),
            (
                "RUSTMAN_EVM_SIGNER",
                rustman(ws.path())
                    .args(args)
                    .env("RUSTMAN_EVM_SIGNER", &value),
            ),
        ] {
            let (code, stdout, stderr) = output(cmd);
            assert_eq!(code, Some(4), "{place}: {stdout}{stderr}");
            assert!(stderr.trim().ends_with(&RAW_KEY_REFUSED[7..]), "{stderr}");
            assert!(
                !format!("{stdout}{stderr}").contains(secret_part(&value)),
                "{place} echoed {value}: {stdout}{stderr}"
            );
        }
    }
    // An id that is not configured (nor key-like) is not echoed either,
    // in the preview or the error.
    let unknown = "Not-A-Signer_But-Private";
    for (place, cmd) in [
        (
            "--signer",
            rustman(ws.path()).args(args).args(["--signer", unknown]),
        ),
        (
            "RUSTMAN_EVM_SIGNER",
            rustman(ws.path())
                .args(args)
                .env("RUSTMAN_EVM_SIGNER", unknown),
        ),
    ] {
        let (code, stdout, stderr) = output(cmd);
        assert_eq!(code, Some(2), "{place}: {stdout}{stderr}");
        assert_eq!(
            stderr.trim(),
            "error: unknown signer (configured: ci, deployer)"
        );
        assert!(
            stdout.contains("signer  (not a configured signer)"),
            "{stdout}"
        );
        assert!(!format!("{stdout}{stderr}").contains(unknown), "{place}");
    }
    // Neither is an id with spaces or a hex string.
    let (code, stdout, stderr) = send(ws.path(), &["--sign", "--signer", "0xdeadbeef"]);
    assert_eq!(code, Some(2), "{stdout}{stderr}");
    assert!(stderr.contains("invalid signer id"), "{stderr}");
    assert!(!format!("{stdout}{stderr}").contains("deadbeef"));
    assert!(node.calls().is_empty(), "nothing is sent");
    assert_no_secret_material(ws.path());
}

#[test]
fn reports_never_hold_a_near_key_or_an_unknown_signer() {
    // Regression: the refusal went into the run's JSON and JUnit reports
    // with the value in it.
    let node = Node::start("0x7a69", Simulation::Succeeds);
    for (signer, env) in [
        (near_keys()[0].clone(), None),
        (near_keys()[2].clone(), None),
        ("deployer".to_owned(), Some(near_keys()[1].clone())),
        (
            "deployer".to_owned(),
            Some("Not-A-Signer_But-Private".to_owned()),
        ),
    ] {
        let ws = workspace(&node.url(), &enabled(), PINNED);
        write(ws.path(), "transfer.rman.yaml", &transfer(&signer));
        let mut cmd = rustman(ws.path());
        cmd.args([
            "run",
            ".",
            "--env",
            "dev",
            "--sign",
            "--yes-local",
            "--report",
            "json=out/report.json",
            "--report",
            "junit=out/report.xml",
        ]);
        let value = env.clone().unwrap_or_else(|| signer.clone());
        if let Some(env) = &env {
            cmd.env("RUSTMAN_EVM_SIGNER", env);
        }
        let (code, stdout, stderr) = output(&mut cmd);
        assert!(matches!(code, Some(3 | 4)), "{code:?}: {stdout}{stderr}");
        let json = std::fs::read_to_string(ws.path().join("out/report.json")).unwrap();
        let junit = std::fs::read_to_string(ws.path().join("out/report.xml")).unwrap();
        assert!(
            json.contains("evm-tx") || json.contains("transfer"),
            "{json}"
        );
        for text in [&json, &junit, &stdout, &stderr] {
            assert!(!text.contains(secret_part(&value)), "{value} in {text}");
        }
        assert_no_secret_material(ws.path());
    }
    assert!(node.calls().is_empty(), "nothing is sent");
}

#[test]
fn a_sent_transaction_whose_receipt_cannot_be_read_is_still_audited() {
    // Regression: the first failing receipt poll ended the send with an
    // RPC error, so a transaction that was sent had no history entry and
    // no explorer link.
    let node = Node::start_with(
        "0x7a69",
        Simulation::Succeeds,
        Broadcast::AcceptsThenReceiptFails,
    );
    let ws = workspace(&node.url(), &enabled(), PINNED);
    set_password(ws.path());
    let (code, stdout, stderr) = send(
        ws.path(),
        &["--sign", "--yes-local", "--receipt-timeout", "2s"],
    );
    assert_eq!(code, Some(3), "{stdout}{stderr}");
    let hash = stdout
        .lines()
        .find_map(|line| line.strip_prefix("sent "))
        .unwrap_or_else(|| panic!("no hash in {stdout}"));
    assert!(
        stderr.contains(&format!(
            "error: transaction {hash} was sent but its receipt could not be read within 2s"
        )),
        "{stderr}"
    );
    assert!(stderr.contains("upstream unavailable"), "{stderr}");
    let polls = node
        .calls()
        .iter()
        .filter(|c| c["method"] == "eth_getTransactionReceipt")
        .count();
    assert!(polls > 1, "the receipt poll is tried again ({polls})");
    // The audit entry: hash, chain, signer, to, nonce; no signed bytes.
    let history = ws.path().join(".rustman/history");
    let entries: Vec<Value> = std::fs::read_dir(&history)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).unwrap()).unwrap())
        .collect();
    assert_eq!(entries.len(), 1, "{entries:?}");
    let audit = &entries[0]["record"]["evm"];
    assert_eq!(audit["hash"], hash, "{audit}");
    assert_eq!(audit["status"], "receipt-unknown", "{audit}");
    assert_eq!(audit["chain_id"], 31337);
    assert_eq!(audit["signer"]["address"], ANVIL0);
    assert_eq!(audit["to"], TOKEN);
    assert_eq!(audit["nonce"], 4);
    let raw = node
        .calls()
        .iter()
        .find(|c| c["method"] == "eth_sendRawTransaction")
        .map(|c| c["params"][0].as_str().unwrap().to_owned())
        .unwrap();
    let text = entries[0].to_string();
    assert!(!text.contains(&raw[2..]), "the signed bytes are not stored");
    assert_no_secret_material(ws.path());
}

/// The history entries under `root`.
fn history_entries(root: &Path) -> Vec<Value> {
    std::fs::read_dir(root.join(".rustman/history"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).unwrap()).unwrap())
        .collect()
}

/// Send through a node that answers the signed transaction with
/// `broadcast`: the broadcast is unknown, so the receipt of the locally
/// computed hash is awaited and history keeps a `broadcast-unknown` audit
/// entry with that hash and no signed bytes. Returns the local hash and
/// stderr.
fn broadcast_unknown(broadcast: Broadcast) -> (String, String) {
    let node = Node::start_with("0x7a69", Simulation::Succeeds, broadcast);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    set_password(ws.path());
    let (code, stdout, stderr) = send(
        ws.path(),
        &["--sign", "--yes-local", "--receipt-timeout", "1s"],
    );
    assert_eq!(code, Some(3), "{stdout}{stderr}");
    let calls = node.calls();
    let sent: Vec<&Value> = calls
        .iter()
        .filter(|c| c["method"] == "eth_sendRawTransaction")
        .collect();
    assert_eq!(sent.len(), 1, "the send is not retried");
    let raw = sent[0]["params"][0].as_str().unwrap().to_owned();
    let hash = raw_hash(&json!(raw));
    assert!(!stdout.contains("sent 0x"), "{stdout}");
    assert!(
        stderr.contains(&format!("error: transaction {hash} may have been sent (")),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "but no receipt came within 1s; check the explorer for {hash} before sending it again"
        )),
        "{stderr}"
    );
    let polled = calls
        .iter()
        .filter(|c| c["method"] == "eth_getTransactionReceipt")
        .all(|c| c["params"][0] == hash);
    assert!(polled, "the receipt of the local hash is awaited");
    let entries = history_entries(ws.path());
    assert_eq!(entries.len(), 1, "{entries:?}");
    let audit = &entries[0]["record"]["evm"];
    assert_eq!(audit["hash"], hash, "{audit}");
    assert_eq!(audit["status"], "broadcast-unknown", "{audit}");
    assert_eq!(audit["chain_id"], 31337);
    assert_eq!(audit["signer"]["address"], ANVIL0);
    assert_eq!(audit["to"], TOKEN);
    assert_eq!(audit["nonce"], 4);
    let text = entries[0].to_string();
    assert!(!text.contains(&raw[2..]), "the signed bytes are not stored");
    assert_no_secret_material(ws.path());
    (hash, stderr)
}

#[test]
fn a_dropped_connection_on_send_is_audited_as_broadcast_unknown() {
    // Regression: a transport failure of eth_sendRawTransaction ended the
    // send with an RPC error and no audit entry, though the transaction
    // may have been broadcast.
    let (_, stderr) = broadcast_unknown(Broadcast::Drops);
    assert!(
        stderr.contains("warning: eth_sendRawTransaction"),
        "{stderr}"
    );
}

#[test]
fn an_rpc_error_on_send_is_audited_as_broadcast_unknown() {
    // Regression: an RPC error answered to eth_sendRawTransaction left no
    // audit entry.
    let (hash, stderr) = broadcast_unknown(Broadcast::Refuses);
    assert!(
        stderr.contains(&format!(
            "warning: {NODE_REFUSED}; transaction {hash} may have been sent; waiting for its receipt"
        )),
        "{stderr}"
    );
    assert_refused_send(&stderr);
}

#[test]
fn another_hash_answered_on_send_is_audited_with_the_local_hash() {
    // Regression: a node answering another hash ended the send with an
    // RPC error and no audit entry.
    let (hash, stderr) = broadcast_unknown(Broadcast::AnswersOtherHash);
    let mismatch =
        format!("eth_sendRawTransaction: the node answered hash {OTHER_HASH}, expected {hash}");
    assert!(
        stderr.contains(&format!("warning: {mismatch}; ")),
        "{stderr}"
    );
    assert!(stderr.contains(&format!("({mismatch})")), "{stderr}");
}

#[cfg(unix)]
#[test]
fn ctrl_c_at_the_confirmation_aborts_with_nothing_sent() {
    // Regression: the prompt was a blocking read on a current-thread
    // runtime, and the first Ctrl+C only cancelled a token nobody looked
    // at, so the transaction was signed and sent after Ctrl+C.
    let node = Node::start_with(
        "0xaa36a7",
        Simulation::Succeeds,
        Broadcast::AcceptsThenReceiptFails,
    );
    let env = "chain_id: 11155111\nsecrets: [deployer_pw]\n";
    let ws = workspace(&node.url(), &enabled(), env);
    set_password(ws.path());
    let args = ["evm", "send", "transfer", "--env", "dev", "--sign"];
    let (code, seen) = on_a_terminal(
        ws.path(),
        &args,
        &[("sign and send this transaction? [y/N] ", "\u{3}y")],
    );
    assert_eq!(code, 130, "{seen}");
    assert!(
        seen.contains("error: interrupted; nothing was signed or sent"),
        "{seen}"
    );
    let methods: Vec<_> = node.calls().iter().map(|c| c["method"].clone()).collect();
    assert!(
        !methods.contains(&json!("eth_sendRawTransaction")),
        "{methods:?}"
    );
}

/// `rustman <args>` in `dir` with the main thread's stack limited to
/// 896 KiB, below Windows' 1 MiB: its exit code and output.
#[cfg(unix)]
fn on_a_small_stack(dir: &Path, args: &str) -> (Option<i32>, String) {
    let bin = assert_cmd::cargo::cargo_bin("rustman");
    let script = format!("ulimit -s 896 && exec '{}' {args}", bin.display());
    let output = std::process::Command::new("sh")
        .args(["-c", &script])
        .current_dir(dir)
        .env("RUSTMAN_SECRET_STORE", "file")
        .env("RUSTMAN_DATA_DIR", common::test_data_dir())
        .env_remove("RUSTMAN_ENV")
        .env_remove("RUSTMAN_EVM_SIGNER")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code(), text)
}

#[cfg(unix)]
#[test]
fn signing_fits_a_small_main_thread_stack() {
    // The gate runs on the CLI's main thread, whose stack is 1 MiB on
    // Windows: `run --sign` (its own boxed future per kind) and
    // `evm send` must get through unlock, simulation, the summary, the
    // signature, the send and the receipt wait within it.
    let node = Node::start_with(
        "0x7a69",
        Simulation::Succeeds,
        Broadcast::AcceptsThenReceiptFails,
    );
    let ws = workspace(&node.url(), &enabled(), PINNED);
    set_password(ws.path());
    let (code, text) = on_a_small_stack(
        ws.path(),
        "evm send transfer --env dev --sign --yes-local --receipt-timeout 1s",
    );
    assert_eq!(code, Some(3), "{text}");
    assert!(text.contains("its receipt could not be read"), "{text}");

    let node = Node::start_with("0x7a69", Simulation::Succeeds, Broadcast::RefusesButMined);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    set_password(ws.path());
    let (code, text) = on_a_small_stack(ws.path(), "run . --env dev --sign --yes-local");
    assert_eq!(code, Some(3), "{text}");
    assert!(text.contains("nonce too low"), "{text}");
    let sent = node
        .calls()
        .iter()
        .filter(|c| c["method"] == "eth_sendRawTransaction")
        .count();
    assert_eq!(sent, 1, "{text}");
}

/// `rustman <args>` in `dir`, running in the background: its output is
/// collected as it comes.
#[cfg(unix)]
struct Running {
    child: std::process::Child,
    stdout: std::sync::Arc<std::sync::Mutex<String>>,
    stderr: std::sync::Arc<std::sync::Mutex<String>>,
}

#[cfg(unix)]
impl Running {
    fn start(dir: &Path, args: &[&str]) -> Self {
        use std::process::Stdio;
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_rustman"))
            .args(args)
            .current_dir(dir)
            .env("RUSTMAN_SECRET_STORE", "file")
            .env("RUSTMAN_DATA_DIR", common::test_data_dir())
            .env_remove("RUSTMAN_ENV")
            .env_remove("RUSTMAN_EVM_SIGNER")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = Self::collect(child.stdout.take().unwrap());
        let stderr = Self::collect(child.stderr.take().unwrap());
        Self {
            child,
            stdout,
            stderr,
        }
    }

    fn collect(
        mut pipe: impl std::io::Read + Send + 'static,
    ) -> std::sync::Arc<std::sync::Mutex<String>> {
        let text = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = std::sync::Arc::clone(&text);
        std::thread::spawn(move || {
            let mut buf = [0_u8; 4096];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        text
    }

    /// Wait until stdout (or stderr) shows `text`.
    fn wait_for(&self, text: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let out = self.stdout.lock().unwrap().clone();
            let err = self.stderr.lock().unwrap().clone();
            if out.contains(text) || err.contains(text) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no '{text}' in {out}{err}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Ctrl+C (SIGINT), then the exit code (within 20 s), stdout and stderr.
    fn interrupt(mut self) -> (Option<i32>, String, String) {
        let pid = self.child.id().to_string();
        let sent = std::process::Command::new("kill")
            .args(["-INT", &pid])
            .status()
            .unwrap();
        assert!(sent.success());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("still running 20 s after Ctrl+C");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        std::thread::sleep(std::time::Duration::from_millis(100));
        let out = self.stdout.lock().unwrap().clone();
        let err = self.stderr.lock().unwrap().clone();
        (status.code(), out, err)
    }
}

/// The hash `rustman` printed as `sent <hash>`.
fn sent_hash(text: &str) -> String {
    text.lines()
        .find_map(|line| line.strip_prefix("sent "))
        .unwrap_or_else(|| panic!("no hash in {text}"))
        .trim()
        .to_owned()
}

/// The one history entry of `root`: a `pending` audit record of `hash`
/// without the signed bytes the node received.
fn assert_pending_audit(root: &Path, node: &Node, hash: &str) {
    let entries = history_entries(root);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let audit = &entries[0]["record"]["evm"];
    assert_eq!(audit["hash"], hash, "{audit}");
    assert_eq!(audit["status"], "pending", "{audit}");
    assert_eq!(audit["chain_id"], 31337);
    assert_eq!(audit["signer"]["address"], ANVIL0);
    let raw = node
        .calls()
        .iter()
        .find(|c| c["method"] == "eth_sendRawTransaction")
        .map(|c| c["params"][0].as_str().unwrap().to_owned())
        .unwrap();
    assert!(
        !entries[0].to_string().contains(&raw[2..]),
        "no signed bytes"
    );
}

#[cfg(unix)]
#[test]
fn ctrl_c_during_the_receipt_wait_audits_the_sent_transaction_and_exits_3() {
    // Regression: the first Ctrl+C during the receipt wait was ignored and
    // the second quit with exit code 130 and no audit entry, although the
    // transaction was in the pool.
    let node = Node::start_with("0x7a69", Simulation::Succeeds, Broadcast::Accepts);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    set_password(ws.path());
    let args = [
        "evm",
        "send",
        "transfer",
        "--env",
        "dev",
        "--sign",
        "--yes-local",
        "--receipt-timeout",
        "120s",
    ];
    let running = Running::start(ws.path(), &args);
    running.wait_for("sent 0x");
    let (code, stdout, stderr) = running.interrupt();
    assert_eq!(code, Some(3), "{stdout}{stderr}");
    let hash = sent_hash(&stdout);
    assert!(
        stderr.contains(&format!(
            "error: interrupted; transaction {hash} was sent and its receipt was not awaited"
        )),
        "{stderr}"
    );
    assert_pending_audit(ws.path(), &node, &hash);
}

#[cfg(unix)]
#[test]
fn ctrl_c_in_a_run_after_the_send_audits_it_and_exits_3() {
    // Regression: `run --sign` dropped the signing future on Ctrl+C after
    // `sent 0x…`: the request was skipped as interrupted, exit code 130,
    // no audit entry.
    let node = Node::start_with("0x7a69", Simulation::Succeeds, Broadcast::Accepts);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    set_password(ws.path());
    let running = Running::start(
        ws.path(),
        &["run", ".", "--env", "dev", "--sign", "--yes-local"],
    );
    running.wait_for("sent 0x");
    let (code, stdout, stderr) = running.interrupt();
    assert_eq!(code, Some(3), "{stdout}{stderr}");
    let hash = sent_hash(&stderr);
    assert!(
        !stderr.contains("skipped (interrupted)"),
        "{stdout}{stderr}"
    );
    assert_pending_audit(ws.path(), &node, &hash);
}

#[cfg(unix)]
#[test]
fn ctrl_c_in_a_run_before_the_send_exits_130_with_nothing_sent() {
    // The gate watches Ctrl+C itself now: before the signature it still
    // stops at once, nothing signed or sent.
    let node = Node::start("0x7a69", Simulation::Slow(30_000));
    let ws = workspace(&node.url(), &enabled(), PINNED);
    set_password(ws.path());
    let running = Running::start(
        ws.path(),
        &["run", ".", "--env", "dev", "--sign", "--yes-local"],
    );
    running.wait_for("evm-tx transfer");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !node
        .calls()
        .iter()
        .any(|c| c["method"] == "eth_estimateGas")
    {
        assert!(std::time::Instant::now() < deadline, "no simulation");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let started = std::time::Instant::now();
    let (code, stdout, stderr) = running.interrupt();
    assert_eq!(code, Some(130), "{stdout}{stderr}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the slow simulation was not awaited"
    );
    let methods: Vec<_> = node.calls().iter().map(|c| c["method"].clone()).collect();
    assert!(
        !methods.contains(&json!("eth_sendRawTransaction")),
        "{methods:?}"
    );
}

#[cfg(unix)]
#[test]
fn an_answer_typed_ahead_never_confirms_an_unseen_summary() {
    // Regression: the prompt read input already buffered, so `y` typed
    // before the summary appeared signed the transaction.
    let node = Node::start("0xaa36a7", Simulation::Slow(1_500));
    let env = "chain_id: 11155111\nsecrets: [deployer_pw]\n";
    let ws = workspace(&node.url(), &enabled(), env);
    set_password(ws.path());
    let args = ["evm", "send", "transfer", "--env", "dev", "--sign"];
    let (code, seen) = on_a_terminal(
        ws.path(),
        &args,
        &[
            // Typed while the simulation runs, before any prompt.
            ("evm-tx transfer", "y"),
            ("sign and send this transaction? [y/N] ", "n"),
        ],
    );
    assert_eq!(code, 4, "{seen}");
    assert!(
        seen.contains("error: cancelled; nothing was signed or sent"),
        "{seen}"
    );
    let methods: Vec<_> = node.calls().iter().map(|c| c["method"].clone()).collect();
    assert!(
        !methods.contains(&json!("eth_sendRawTransaction")),
        "{methods:?}"
    );
}

#[test]
fn the_confirmation_shows_values_a_short_secret_would_redact() {
    // Regression: the summary went through the run's substring redactor,
    // so a secret equal to part of an address mangled the address the
    // user confirmed (`0x«redacted:…»9979…`).
    let node = Node::start("0x7a69", Simulation::Succeeds);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    let env = "chain_id: 31337\nsecrets: [deployer_pw, node_key]\n";
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!("{env}variables:\n  rpc: {}\n", node.url()),
    );
    let tx = transfer("deployer").replace("rpc: \"{{rpc}}\"", "rpc: \"{{rpc}}?key={{node_key}}\"");
    write(ws.path(), "transfer.rman.yaml", &tx);
    set_password(ws.path());
    rustman(ws.path())
        .args(["secret", "set", "node_key", "--env", "dev"])
        .write_stdin("70997970")
        .assert()
        .success();
    let (code, stdout, stderr) = send(
        ws.path(),
        &["--sign", "--yes-local", "--receipt-timeout", "1s"],
    );
    assert_eq!(code, Some(3), "{stdout}{stderr}");
    let summary = &stdout[stdout
        .find("  chain ")
        .unwrap_or_else(|| panic!("{stdout}"))..];
    let call = summary.lines().find(|l| l.starts_with("  call ")).unwrap();
    assert!(call.contains(BOB), "{call}");
    assert!(!summary.contains("redacted"), "{summary}");
    assert!(!stdout.contains(PASSWORD) && !stderr.contains(PASSWORD));
}

#[test]
fn a_transaction_field_from_a_secret_is_refused() {
    // Regression: an argument from a secret was shown as
    // `amount=«redacted:…»` and signed under --yes-local.
    let node = Node::start("0x7a69", Simulation::Succeeds);
    let ws = workspace(&node.url(), &enabled(), PINNED);
    let env = "chain_id: 31337\nsecrets: [deployer_pw, amount]\n";
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!("{env}variables:\n  rpc: {}\n", node.url()),
    );
    let tx = transfer("deployer").replace("\"10\"]", "\"{{amount}}\"]");
    write(ws.path(), "transfer.rman.yaml", &tx);
    set_password(ws.path());
    rustman(ws.path())
        .args(["secret", "set", "amount", "--env", "dev"])
        .write_stdin("10")
        .assert()
        .success();
    let (code, stdout, stderr) = send(ws.path(), &["--sign", "--yes-local"]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(
        stderr.contains(
            "error: evm-tx: 'args' comes from a secret; a transaction field must be shown in the confirmation, so it cannot hold a secret; nothing was signed or sent"
        ),
        "{stderr}"
    );
    let methods: Vec<_> = node.calls().iter().map(|c| c["method"].clone()).collect();
    assert!(
        !methods.contains(&json!("eth_sendRawTransaction")),
        "{methods:?}"
    );
}
