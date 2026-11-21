//! ENS resolution against recorded mainnet answers (task 3.4): forward,
//! reverse verified by a forward lookup, and the mainnet requirement.

use std::path::PathBuf;

use alloy_primitives::address;
use rustman_evm::Recorded;
use rustman_evm::ens::{ENS_NEEDS_MAINNET, Ens, ReverseName, ens_endpoint, ens_for};

fn mainnet() -> Recorded {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mainnet/ens.json");
    Recorded::from_file("https://mainnet.test/", &path).unwrap()
}

#[tokio::test]
async fn forward_resolution_gives_the_checksummed_address() {
    let rpc = mainnet();
    let resolved = Ens::new(&rpc).resolve("vitalik.eth").await.unwrap();
    assert_eq!(
        resolved.to_checksum(None),
        "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
    );
    assert_eq!(rpc.methods(), ["eth_call", "eth_call"]);
}

#[tokio::test]
async fn reverse_resolution_is_verified_forward() {
    let rpc = mainnet();
    let vitalik = address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045");
    let name = Ens::new(&rpc).reverse(vitalik).await.unwrap();
    assert_eq!(name, ReverseName::Verified("vitalik.eth".into()));
    assert_eq!(rpc.methods().len(), 4, "reverse, name, then forward");
}

#[tokio::test]
async fn a_reverse_name_that_resolves_elsewhere_is_not_verified() {
    let other = address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045");
    // A recorded reverse record, but for an address the name does not
    // resolve to: the fixture's forward answer is vitalik's address.
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mainnet/ens.json"),
    )
    .unwrap()
    .replace(
        "000000000000000000000000d8da6bf26964af9d7eed9e03e53415d37aa96045",
        "0000000000000000000000000000000000000000000000000000000000000001",
    );
    let lying = Recorded::from_json("https://mainnet.test/", &text).unwrap();
    let name = Ens::new(&lying).reverse(other).await.unwrap();
    assert_eq!(name, ReverseName::Unverified("vitalik.eth".into()));
}

#[tokio::test]
async fn an_unregistered_name_and_an_address_without_reverse_record() {
    let rpc = mainnet();
    let ens = Ens::new(&rpc);
    // Any other registry read answers zero, as for an unregistered name.
    let err = ens.resolve("nobody-here.eth").await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "ENS name 'nobody-here.eth' has no resolver"
    );
    assert_eq!(err.exit_code(), 1);
    let unknown = address!("0x0000000000000000000000000000000000000001");
    assert_eq!(ens.reverse(unknown).await.unwrap(), ReverseName::None);
    let hex = ens
        .address("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
        .await
        .unwrap();
    assert_eq!(hex, address!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"));
}

#[test]
fn names_need_a_mainnet_endpoint() {
    let sepolia = Recorded::new("https://sepolia.test/", Vec::new());
    let err = ens_endpoint(&sepolia, 11_155_111, None).err().unwrap();
    assert_eq!(
        err.to_string(),
        "ENS needs a mainnet RPC; set [evm] ens_rpc"
    );
    assert_eq!(err.to_string(), ENS_NEEDS_MAINNET);
    let main = mainnet();
    let chosen = ens_endpoint(&sepolia, 11_155_111, Some(&main)).unwrap();
    assert_eq!(chosen.endpoint(), "https://mainnet.test/");
    let chosen = ens_endpoint(&main, 1, None).unwrap();
    assert_eq!(chosen.endpoint(), "https://mainnet.test/");
}

#[tokio::test]
async fn a_configured_ens_rpc_on_another_chain_is_refused() {
    // Regression: an `[evm] ens_rpc` pointed at Sepolia (or a hostile
    // node) was read as mainnet, giving other addresses without a word.
    let sepolia = Recorded::from_json(
        "https://sepolia.test/",
        r#"[{"method": "eth_chainId", "result": "0xaa36a7"}]"#,
    )
    .unwrap();
    let current = Recorded::new("https://current.test/", Vec::new());
    let ens = ens_for(&current, 11_155_111, Some(&sepolia)).unwrap();
    let err = ens.resolve("vitalik.eth").await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "[evm] ens_rpc reports chain 11155111, not Ethereum Mainnet (1); ENS is read on mainnet only"
    );
    assert_eq!(err.exit_code(), 4);
    assert_eq!(sepolia.methods(), ["eth_chainId"], "no registry read");
    // A mainnet ens_rpc is checked once, then read.
    let main = mainnet();
    let ens = ens_for(&current, 11_155_111, Some(&main)).unwrap();
    ens.resolve("vitalik.eth").await.unwrap();
    ens.resolve("vitalik.eth").await.unwrap();
    let methods = main.methods();
    assert_eq!(methods.iter().filter(|m| *m == "eth_chainId").count(), 1);
}
