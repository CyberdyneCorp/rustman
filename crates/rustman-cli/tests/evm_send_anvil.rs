//! `rustman evm send` end to end against a local Anvil (tasks 5.3-5.5):
//! two sequential transactions signed with `--yes-local` without a
//! terminal, the receipt's assertions and captures, the history audit
//! entries scanned for key material and signed bytes, a receipt timeout
//! (exit code 3), and a confirmation answered on a terminal.
//!
//! Anvil's first dev account, from an encrypted test keystore; the fixture
//! contract's runtime code is installed with `anvil_setCode`. These tests
//! only run with `RUSTMAN_ANVIL=1` (or `RUSTMAN_REQUIRE_ANVIL=1`, the CI
//! Anvil job, where a missing `anvil` fails them). Nothing leaves loopback.

mod common;

mod anvil_node;

use std::path::Path;

use anvil_node::{Node, PASSWORD, anvil_program, signing_fixtures};
use common::{rustman, write};
use serde_json::{Value, json};

const COUNTER: &str = "0x00000000000000000000000000000000C0C0C0C0";
const ANVIL0: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
const ANVIL0_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

fn add_request(amount: u64, gas_lt: u64) -> String {
    format!(
        "version: 1\nkind: evm-tx\nname: add\nrpc: \"{{{{rpc}}}}\"\nto: \"{COUNTER}\"\n\
         abi: Counter.abi.json\nfunction: add\nargs: [\"{amount}\"]\nsigner: deployer\n\
         assertions:\n  - evm: {{emits: \"Added(address,uint256,uint256)\", where: {{amount: \"{amount}\"}}}}\n\
         \x20 - evm: {{gas_lt: {gas_lt}}}\n\
         captures:\n  - {{var: mined_in, from: evm_receipt, field: blockNumber, persist: true}}\n"
    )
}

/// A workspace that signs on the local `dev` environment (chain 31337).
fn workspace(url: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n\n[evm]\nallow_signing = true\n\
         [evm.signers.deployer]\nkeystore = \"keys/anvil0.json\"\npassword = \"{{deployer_pw}}\"\n",
    );
    write(
        dir.path(),
        "environments/dev.yaml",
        &format!("chain_id: 31337\nsecrets: [deployer_pw]\nvariables:\n  rpc: {url}\n"),
    );
    write(dir.path(), ".gitignore", ".rustman/\nkeys/\n");
    let fixtures = signing_fixtures();
    let read = |rel: &str| std::fs::read_to_string(fixtures.join(rel)).unwrap();
    write(
        dir.path(),
        "keys/anvil0.json",
        &read("keystores/anvil0.json"),
    );
    write(
        dir.path(),
        "Counter.abi.json",
        &read("counter/Counter.abi.json"),
    );
    write(dir.path(), "add.rman.yaml", &add_request(2, 100_000));
    rustman(dir.path())
        .args(["secret", "set", "deployer_pw", "--env", "dev"])
        .write_stdin(PASSWORD)
        .assert()
        .success();
    dir
}

fn send(dir: &Path, extra: &[&str]) -> (Option<i32>, String, String) {
    let mut args = vec!["evm", "send", "add", "--env", "dev", "--sign"];
    args.extend(extra);
    let out = rustman(dir).args(args).output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The hash printed after `sent `.
fn sent_hash(stdout: &str) -> String {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("sent "))
        .unwrap_or_else(|| panic!("no hash in {stdout}"))
        .to_owned()
}

/// The `evm` audit records of the history, oldest first.
fn audits(root: &Path) -> Vec<Value> {
    let mut entries: Vec<(String, Value)> = std::fs::read_dir(root.join(".rustman/history"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .map(|e| {
            let text = std::fs::read_to_string(e.path()).unwrap();
            let entry: Value = serde_json::from_str(&text).unwrap();
            (entry["id"].as_str().unwrap_or_default().to_owned(), entry)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.into_iter().map(|(_, entry)| entry).collect()
}

/// No file under `.rustman/` (but the test secret store) holds the key,
/// the password or any of the `signed` transactions' bytes.
fn assert_nothing_secret(root: &Path, signed: &[String]) {
    let mut stack = vec![root.join(".rustman")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.file_name().is_some_and(|n| n == "secrets.yaml") {
                continue;
            }
            let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).to_lowercase();
            assert!(!text.contains(ANVIL0_KEY), "{}", path.display());
            assert!(!text.contains(PASSWORD), "{}", path.display());
            for raw in signed {
                let raw = raw.trim_start_matches("0x").to_lowercase();
                assert!(
                    !text.contains(&raw),
                    "{} holds signed bytes",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn sequential_sends_are_signed_mined_checked_and_audited() {
    // Scenarios "CI against Anvil", "Sequential transactions in a run"
    // (one signer, increasing nonces) and "Audit record".
    let Some(program) = anvil_program() else {
        return;
    };
    let node = Node::start(&program, COUNTER);
    let ws = workspace(&node.url());

    let (code, stdout, stderr) = send(ws.path(), &["--yes-local"]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(
        stderr.contains("confirmed by --yes-local on Anvil/Hardhat local (31337)"),
        "{stderr}"
    );
    assert!(stdout.contains("max fee"), "{stdout}");
    assert!(stdout.contains("(suggested)"), "{stdout}");
    assert!(stdout.contains("nonce        0"), "{stdout}");
    assert!(stdout.contains("status     success"), "{stdout}");
    assert!(stdout.contains("Added(by="), "{stdout}");
    assert!(
        !stdout.contains("explorer:"),
        "a local chain has no explorer"
    );
    let first = sent_hash(&stdout);

    write(ws.path(), "add.rman.yaml", &add_request(3, 100_000));
    let (code, stdout, stderr) = send(ws.path(), &["--yes-local", "--confirmations", "1"]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("nonce        1"), "{stdout}");
    let second = sent_hash(&stdout);

    let number = node.call(
        "eth_call",
        json!([{"to": COUNTER, "data": "0x8381f58a"}, "latest"]),
    );
    assert_eq!(number, json!(format!("0x{:0>64}", "5")));

    let audits = audits(ws.path());
    assert_eq!(audits.len(), 2, "{audits:?}");
    for (entry, hash) in audits.iter().zip([&first, &second]) {
        let evm = &entry["record"]["evm"];
        assert_eq!(evm["kind"], "evm-tx");
        assert_eq!(evm["hash"], **hash);
        assert_eq!(evm["chain_id"], 31_337);
        assert_eq!(evm["signer"]["address"], ANVIL0);
        assert_eq!(evm["status"], "success");
        assert!(evm["gas_used"].as_str().is_some(), "{evm}");
        assert!(evm["call"].as_str().unwrap().starts_with("add("), "{evm}");
        assert_eq!(entry["replay"], Value::Null, "never replayable");
        assert_eq!(entry["record"]["status"], "passed", "{entry}");
    }
    let captured = &audits[1]["record"]["captures"];
    assert!(captured.to_string().contains("mined_in"), "{captured}");

    let signed: Vec<String> = [&first, &second]
        .iter()
        .map(|hash| {
            let raw = node.call("eth_getRawTransactionByHash", json!([hash]));
            raw.as_str().unwrap().to_owned()
        })
        .collect();
    assert!(signed.iter().all(|raw| raw.len() > 100));
    assert_nothing_secret(ws.path(), &signed);
}

#[test]
fn a_failed_receipt_assertion_exits_with_one() {
    let Some(program) = anvil_program() else {
        return;
    };
    let node = Node::start(&program, COUNTER);
    let ws = workspace(&node.url());
    write(ws.path(), "add.rman.yaml", &add_request(4, 1_000));
    let (code, stdout, stderr) = send(ws.path(), &["--yes-local"]);
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(
        stderr.contains("fail: evm gas_lt: expected < 1000"),
        "{stderr}"
    );
    assert_eq!(audits(ws.path())[0]["record"]["status"], "failed");
}

#[test]
fn a_receipt_that_never_comes_times_out_with_exit_code_3() {
    // Scenario "Receipt timeout", with a short timeout.
    let Some(program) = anvil_program() else {
        return;
    };
    let node = Node::start(&program, COUNTER);
    node.call("evm_setAutomine", json!([false]));
    let ws = workspace(&node.url());
    let (code, stdout, stderr) = send(ws.path(), &["--yes-local", "--receipt-timeout", "1s"]);
    assert_eq!(code, Some(3), "{stdout}{stderr}");
    let hash = sent_hash(&stdout);
    assert!(
        stderr.contains(&format!(
            "error: transaction {hash} was sent but not mined within 1s"
        )),
        "{stderr}"
    );
    let audits = audits(ws.path());
    assert_eq!(audits[0]["record"]["evm"]["status"], "pending");
    assert_eq!(audits[0]["record"]["evm"]["hash"], hash);
}

#[cfg(unix)]
#[test]
fn a_terminal_confirmation_signs_on_yes() {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let Some(program) = anvil_program() else {
        return;
    };
    let node = Node::start(&program, COUNTER);
    let ws = workspace(&node.url());
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_rustman"));
    cmd.args(["evm", "send", "add", "--env", "dev", "--sign"]);
    cmd.cwd(ws.path());
    cmd.env("RUSTMAN_SECRET_STORE", "file");
    cmd.env("RUSTMAN_DATA_DIR", ws.path().join(".data"));
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
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let reading = std::thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
        }
    });
    let mut seen = String::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !seen.contains("sign and send this transaction? [y/N] ") {
        assert!(Instant::now() < deadline, "no prompt in {seen}");
        if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
            seen.push_str(&chunk);
        }
    }
    writer.write_all(b"y\r").unwrap();
    writer.flush().unwrap();
    let status = child.wait().unwrap();
    drop(writer);
    drop(pty.master);
    let _ = reading.join();
    seen.extend(rx.try_iter());
    assert_eq!(status.exit_code(), 0, "{seen}");
    assert!(seen.contains("status     success"), "{seen}");
    assert_eq!(audits(ws.path()).len(), 1);
}

#[test]
fn a_run_with_sign_and_yes_local_signs_every_evm_tx_with_increasing_nonces() {
    // Scenarios "CI against Anvil" and "Sequential transactions in a run"
    // through `rustman run` itself. Regression: `rustman run` had no
    // --sign, so evm-tx files could only be skipped.
    let Some(program) = anvil_program() else {
        return;
    };
    let node = Node::start(&program, COUNTER);
    let ws = workspace(&node.url());
    write(ws.path(), "add2.rman.yaml", &add_request(3, 100_000));
    let out = rustman(ws.path())
        .args(["run", ".", "--env", "dev", "--sign", "--yes-local"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stdout}{stderr}");
    assert!(stderr.contains("nonce        0"), "{stderr}");
    assert!(stderr.contains("nonce        1"), "{stderr}");
    assert!(
        stdout.contains("2 passed, 0 failed, 0 errored, 0 skipped"),
        "{stdout}"
    );
    let number = node.call(
        "eth_call",
        json!([{"to": COUNTER, "data": "0x8381f58a"}, "latest"]),
    );
    assert_eq!(number, json!(format!("0x{:0>64}", "5")));
    let audits = audits(ws.path());
    assert_eq!(audits.len(), 2, "{audits:?}");
    let nonces: Vec<_> = audits
        .iter()
        .map(|a| a["record"]["evm"]["nonce"].clone())
        .collect();
    assert_eq!(nonces, [json!(0), json!(1)], "{audits:?}");
}
