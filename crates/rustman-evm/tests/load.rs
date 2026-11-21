//! Loading `evm-call` and `evm-tx` files (task 1.2) and their ABI sources
//! (task 2.2): every source format, overloads, `to`, path escapes, and no
//! node or signer access while loading.

use std::fs;
use std::path::{Path, PathBuf};

use rustman_core::model::{DEPLOYMENT_OUT_OF_SCOPE, KindRegistry, Request};
use rustman_core::{ConfigError, Workspace};
use rustman_evm::abi::{AbiCache, AbiOrigin};
use rustman_evm::load::{check, check_workspace};
use rustman_test_support::jsonrpc::{JsonrpcHttpServer, RpcReply};
use serde_json::json;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/abi")
        .join(name)
}

/// A workspace with the Vault ABI as a plain file, a Foundry artifact and a
/// Hardhat artifact.
fn workspace(rpc: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let toml = format!(
        "[workspace]\nname = \"evm\"\nversion = 1\n\n[variables]\nrpc = \"{rpc}\"\n\n[evm.signers.deployer]\nkeystore = \"keys/missing.json\"\n"
    );
    fs::write(root.join("rustman.toml"), toml).unwrap();
    for (from, to) in [
        ("Vault.abi.json", "abi/Vault.json"),
        ("foundry-Vault.json", "out/Vault.sol/Vault.json"),
        (
            "hardhat-Vault.json",
            "artifacts/contracts/Vault.sol/Vault.json",
        ),
    ] {
        let target = root.join(to);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(fixture(from), target).unwrap();
    }
    fs::create_dir_all(root.join("requests")).unwrap();
    dir
}

fn write(root: &Path, name: &str, text: &str) {
    fs::write(root.join("requests").join(name), text).unwrap();
}

fn call(abi: &str, function: &str, args: &str) -> String {
    format!(
        "version: 1\nkind: evm-call\nname: c\nrpc: \"{{{{rpc}}}}\"\nto: \"0x5FbDB2315678afecb367f032d93F642f64180aa3\"\nabi: {abi}\nfunction: \"{function}\"\nargs: {args}\n"
    )
}

fn checked(root: &Path, name: &str) -> Result<rustman_evm::load::CheckedRequest, String> {
    let file = Path::new("requests").join(name);
    let text = fs::read_to_string(root.join(&file)).unwrap();
    let request = KindRegistry::default()
        .load(&file, &text)
        .map_err(|e| e.to_string())?;
    let Request::Evm(evm) = request else {
        panic!("not an evm request");
    };
    check(root, &file, &evm, &AbiCache::new()).map_err(|e| e.to_string())
}

#[test]
fn every_abi_source_format_loads() {
    let dir = workspace("http://127.0.0.1:1");
    let root = dir.path();
    let cases = [
        (
            "builtin.rman.yaml",
            "erc20",
            "balanceOf",
            AbiOrigin::BuiltIn("erc20"),
        ),
        (
            "json.rman.yaml",
            "../abi/Vault.json",
            "balance",
            AbiOrigin::File(PathBuf::from("abi/Vault.json")),
        ),
        (
            "foundry.rman.yaml",
            "../out/Vault.sol/Vault.json",
            "balance",
            AbiOrigin::File(PathBuf::from("out/Vault.sol/Vault.json")),
        ),
        (
            "hardhat.rman.yaml",
            "../artifacts/contracts/Vault.sol/Vault.json",
            "balance",
            AbiOrigin::File(PathBuf::from("artifacts/contracts/Vault.sol/Vault.json")),
        ),
    ];
    for (name, abi, function, origin) in cases {
        let args = if function == "balanceOf" {
            "[\"0x5FbDB2315678afecb367f032d93F642f64180aa3\"]"
        } else {
            "[]"
        };
        write(root, name, &call(abi, function, args));
        let checked = checked(root, name).unwrap_or_else(|e| panic!("{name}: {e}"));
        let loaded = checked.abi.unwrap();
        assert_eq!(loaded.origin, origin, "{name}");
        assert_eq!(checked.function.unwrap().name, function);
    }
}

#[test]
fn identical_abi_bytes_are_parsed_once() {
    let dir = workspace("http://127.0.0.1:1");
    let root = dir.path();
    fs::copy(fixture("Vault.abi.json"), root.join("abi/Copy.json")).unwrap();
    let cache = AbiCache::new();
    let file = Path::new("requests/x.rman.yaml");
    let a = cache.load(root, file, "../abi/Vault.json").unwrap();
    let b = cache.load(root, file, "../abi/Copy.json").unwrap();
    assert!(std::sync::Arc::ptr_eq(&a.abi, &b.abi));
    assert_eq!(cache.len(), 1);
}

#[test]
fn an_overloaded_function_fails_to_load() {
    let dir = workspace("http://127.0.0.1:1");
    let root = dir.path();
    write(
        root,
        "t.rman.yaml",
        &call(
            "../abi/Vault.json",
            "transfer",
            "[\"0x5FbDB2315678afecb367f032d93F642f64180aa3\", \"1\"]",
        ),
    );
    let err = checked(root, "t.rman.yaml").unwrap_err();
    assert!(
        err.ends_with("function 'transfer' is overloaded; use a full signature such as 'transfer(address,uint256)'"),
        "{err}"
    );
    assert!(
        err.starts_with(&format!(
            "requests{}t.rman.yaml:7",
            std::path::MAIN_SEPARATOR
        )),
        "{err}"
    );
    write(
        root,
        "t2.rman.yaml",
        &call(
            "../abi/Vault.json",
            "transfer(address,uint256)",
            "[\"0x5FbDB2315678afecb367f032d93F642f64180aa3\", \"1\"]",
        ),
    );
    assert!(checked(root, "t2.rman.yaml").is_ok());
}

#[test]
fn argument_counts_and_literals_are_checked() {
    let dir = workspace("http://127.0.0.1:1");
    let root = dir.path();
    write(root, "a.rman.yaml", &call("erc20", "balanceOf", "[]"));
    let err = checked(root, "a.rman.yaml").unwrap_err();
    assert!(
        err.ends_with("function 'balanceOf(address)' takes 1 argument(s), got 0"),
        "{err}"
    );
    let bad_value = format!("{}value: 1.5 finney\n", call("erc20", "totalSupply", "[]"));
    write(root, "v.rman.yaml", &bad_value);
    let err = checked(root, "v.rman.yaml").unwrap_err();
    assert!(
        err.contains("value: invalid quantity '1.5 finney': unknown unit 'finney'"),
        "{err}"
    );
    let bad_block = format!("{}block: newest\n", call("erc20", "totalSupply", "[]"));
    write(root, "b.rman.yaml", &bad_block);
    assert!(
        checked(root, "b.rman.yaml")
            .unwrap_err()
            .contains("block: 'newest'")
    );
    let overrides = format!(
        "{}state_overrides:\n  \"0xabc\": {{balance: 1 ether}}\n",
        call("erc20", "totalSupply", "[]")
    );
    write(root, "o.rman.yaml", &overrides);
    assert!(
        checked(root, "o.rman.yaml")
            .unwrap_err()
            .contains("not a 20-byte hex address")
    );
    let templated = format!(
        "{}value: \"{{{{amount}}}}\"\nblock: \"{{{{block}}}}\"\n",
        call("erc20", "totalSupply", "[]")
    );
    write(root, "t.rman.yaml", &templated);
    assert!(checked(root, "t.rman.yaml").is_ok());
}

#[test]
fn abi_paths_cannot_escape_the_workspace() {
    let dir = workspace("http://127.0.0.1:1");
    let root = dir.path();
    // Named after this test's workspace: parallel test processes share the
    // parent directory.
    let name = format!(
        "outside-{}.json",
        root.file_name().unwrap().to_string_lossy()
    );
    let outside = root.parent().unwrap().join(&name);
    fs::copy(fixture("Vault.abi.json"), &outside).unwrap();
    let escaping = format!("../../{name}");
    write(root, "e.rman.yaml", &call(&escaping, "balance", "[]"));
    let err = checked(root, "e.rman.yaml").unwrap_err();
    fs::remove_file(outside).unwrap();
    assert!(
        err.ends_with(&format!(
            "abi path '{escaping}' resolves outside the workspace"
        )),
        "{err}"
    );
    write(
        root,
        "m.rman.yaml",
        &call("../abi/Missing.json", "balance", "[]"),
    );
    assert!(
        checked(root, "m.rman.yaml")
            .unwrap_err()
            .contains("abi '../abi/Missing.json': cannot read")
    );
}

#[test]
fn an_evm_tx_without_to_is_a_deployment() {
    let text = "version: 1\nkind: evm-tx\nname: deploy\nrpc: http://x\nabi: erc20\nfunction: transfer\nargs: []\n";
    let err = KindRegistry::default()
        .load(Path::new("d.rman.yaml"), text)
        .unwrap_err();
    assert_eq!(err.message, DEPLOYMENT_OUT_OF_SCOPE);
    assert_eq!(
        err.message,
        "contract deployment is out of scope; use Foundry or Hardhat"
    );
}

#[tokio::test]
async fn loading_a_workspace_calls_no_node_and_no_signer() {
    let server = JsonrpcHttpServer::start(|_, _| {
        RpcReply::json(&json!({"jsonrpc": "2.0", "id": 1, "result": "0x1"}))
    })
    .await;
    let dir = workspace(&server.url("/"));
    let root = dir.path();
    write(
        root,
        "call.rman.yaml",
        &call("../out/Vault.sol/Vault.json", "balance", "[]"),
    );
    let tx = "version: 1\nkind: evm-tx\nname: send\nrpc: \"{{rpc}}\"\nto: bob.eth\nabi: erc20\nfunction: transfer\nargs: [bob.eth, \"10\"]\nsigner: deployer\nmax_fee: 30 gwei\n";
    write(root, "send.rman.yaml", tx);
    write(
        root,
        "explorer.rman.yaml",
        &call("{explorer: true}", "balance", "[]"),
    );
    let ws = Workspace::load(root).unwrap();
    assert_eq!(ws.requests().len(), 3);
    assert!(check_workspace(&ws, &AbiCache::new()).is_empty());
    assert!(server.hits().is_empty());
}

#[test]
fn workspace_loading_reports_evm_schema_errors() {
    let dir = workspace("http://127.0.0.1:1");
    let root = dir.path();
    write(
        root,
        "bad.rman.yaml",
        "version: 1\nkind: evm-call\nname: x\nrpc: http://x\nto: \"0x1\"\nabi: erc20\nfunction: name\ngas: 1\n",
    );
    let Err(ConfigError::Invalid(errors)) = Workspace::load(root) else {
        panic!("expected load errors");
    };
    assert!(
        errors.iter().any(|e| e.message.contains("gas")),
        "{errors:?}"
    );
}
