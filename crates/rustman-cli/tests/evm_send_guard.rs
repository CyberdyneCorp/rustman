//! The send-method blocklist (add-evm-toolkit task 3.6): a `jsonrpc` file
//! calling `eth_sendRawTransaction` is refused with exit code 4 by
//! `rustman rpc call` and `rustman run`, and the node receives nothing.

mod common;

use common::{rustman, write};
use rustman_test_support::{JsonrpcHttpServer, RpcReply};
use serde_json::json;

const REFUSED: &str = "signing and sending go through 'evm-tx' (signing gate)";

#[test]
fn a_raw_send_is_refused_and_nothing_reaches_the_node() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = runtime.block_on(JsonrpcHttpServer::start(|_, _| {
        RpcReply::json(&json!({"jsonrpc": "2.0", "id": 1, "result": "0x1"}))
    }));
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    let file = format!(
        "version: 1\nkind: jsonrpc\nname: Raw send\nendpoint: '{}'\nmethod: eth_sendRawTransaction\nparams: [\"0x02f8\"]\n",
        server.url("/")
    );
    write(dir.path(), "send.rman.yaml", &file);
    for args in [
        vec!["rpc", "call", "send.rman.yaml", "--no-history"],
        vec!["run", "send.rman.yaml", "--no-history"],
    ] {
        let out = rustman(dir.path()).args(&args).output().unwrap();
        let shown = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.status.code(), Some(4), "{args:?}: {shown}");
        assert!(shown.contains(REFUSED), "{args:?}: {shown}");
    }
    let out = rustman(dir.path())
        .args([
            "rpc",
            "call",
            &server.url("/"),
            "personal_sign",
            "[]",
            "--no-history",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    assert!(server.hits().is_empty(), "nothing is sent");
    // Regression: eth_sendTransactionSync and the other sending families
    // passed the exact-name list, and Anvil signed and mined them.
    for method in [
        "eth_sendTransactionSync",
        "eth_sendRawTransactionSync",
        "eth_sendRawTransactionConditional",
        "eth_sendBundle",
        "wallet_sendCalls",
        "personal_unlockAccount",
    ] {
        let out = rustman(dir.path())
            .args([
                "rpc",
                "call",
                &server.url("/"),
                method,
                "[]",
                "--no-history",
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(4), "{method}");
    }
    assert!(server.hits().is_empty(), "nothing is sent");
}
