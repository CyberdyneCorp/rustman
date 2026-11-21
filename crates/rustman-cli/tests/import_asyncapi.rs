//! `rustman import asyncapi` (add-messaging-client tasks 4.1 and 4.2)
//! end to end through the binary: environments and request files from a
//! 3.0 document, the unsupported-version exit code, and kept files.

mod common;

use std::path::Path;

use common::{rustman, write};
use predicates::str::contains;

const SHOP_30: &str = "\
asyncapi: 3.0.0
info:
  title: Shop Events
  version: 1.0.0
servers:
  production:
    host: kafka.shop.example:9092
    protocol: kafka
channels:
  orders:
    address: orders
    messages:
      order:
        name: OrderCreated
        payload:
          type: object
          required: [id]
          properties:
            id: {type: integer}
        examples:
          - payload: {id: 7}
operations:
  sendOrder:
    action: send
    channel:
      $ref: '#/channels/orders'
";

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    dir
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

#[test]
fn a_3_0_document_becomes_an_environment_and_request_files() {
    let ws = workspace();
    write(ws.path(), "shop.yaml", SHOP_30);
    rustman(ws.path())
        .args(["import", "asyncapi", "shop.yaml"])
        .assert()
        .success()
        .stdout(contains("imported 1 operation into shop-events"));
    let request = read(ws.path(), "shop-events/orders/send-order.rman.yaml");
    assert!(request.contains("kind: kafka\n"), "{request}");
    assert!(request.contains("operation: produce\n"), "{request}");
    assert!(request.contains("  - value: {\"id\":7}\n"), "{request}");
    let env = read(ws.path(), "environments/production.yaml");
    assert!(
        env.contains("brokers: \"kafka.shop.example:9092\""),
        "{env}"
    );
    assert!(ws.path().join("shop-events/asyncapi.json").exists());
    rustman(ws.path())
        .args(["import", "asyncapi", "shop.yaml", "--into", "events"])
        .assert()
        .success()
        .stderr(contains(
            "production.yaml already exists and was not changed",
        ));
    assert!(
        ws.path()
            .join("events/orders/send-order.rman.yaml")
            .exists()
    );
}

#[test]
fn an_asyncapi_2_0_document_exits_with_code_2() {
    let ws = workspace();
    write(
        ws.path(),
        "old.yaml",
        "asyncapi: '2.0.0'\ninfo: {title: Old, version: '1'}\nchannels: {}\n",
    );
    rustman(ws.path())
        .args(["import", "asyncapi", "old.yaml"])
        .assert()
        .code(2)
        .stderr(contains(
            "asyncapi: unsupported version 2.0 (supported: 2.6, 3.0)",
        ));
}
