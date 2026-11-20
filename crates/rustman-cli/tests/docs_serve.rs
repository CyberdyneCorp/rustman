//! `rustman docs serve` end to end (api-docs-generation "Live-reload
//! preview"): it serves the built site on 127.0.0.1 with the reload script,
//! keeps secrets from the store out of pages and the terminal, and a busy
//! port exits 2 naming the port.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::{rustman, write};

const TOKEN: &str = "CANARY-serve-5d19a2";

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"Shop\"\nversion = 1\n",
    );
    write(
        root,
        "environments/dev.yaml",
        "variables:\n  base_url: http://localhost:9\nsecrets:\n  - token\n",
    );
    write(
        root,
        "users/list.rman.yaml",
        "version: 1\nkind: http\nname: List users\nurl: '{{base_url}}/users'\nheaders:\n  - name: Authorization\n    value: Bearer {{token}}\n",
    );
    let stored = rustman(root)
        .args(["secret", "set", "token", "--env", "dev"])
        .write_stdin(format!("{TOKEN}\n"))
        .output()
        .unwrap();
    assert!(stored.status.success());
    dir
}

fn spawn_serve(dir: &Path, args: &[&str]) -> Child {
    std::process::Command::new(assert_cmd::cargo::cargo_bin("rustman"))
        .current_dir(dir)
        .env("RUSTMAN_SECRET_STORE", "file")
        .env_remove("RUSTMAN_ENV")
        .args(["docs", "serve"])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// The first line of the child's stdout (killed if it never comes).
fn first_line(child: &mut Child) -> String {
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    rx.recv_timeout(Duration::from_secs(60))
        .unwrap_or_else(|_| {
            let _ = child.kill();
            panic!("no banner");
        })
}

fn get(addr: &str, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    let head = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn serve_prints_its_loopback_address_and_serves_the_site() {
    let dir = workspace();
    let mut child = spawn_serve(dir.path(), &["--port", "0", "--env", "dev"]);
    let banner = first_line(&mut child);
    let addr = banner
        .trim_end()
        .strip_prefix("docs: serving on http://")
        .unwrap_or_else(|| panic!("unexpected banner {banner:?}"))
        .to_owned();
    assert!(addr.starts_with("127.0.0.1:"), "{addr}");
    let index = get(&addr, "/");
    assert!(index.starts_with("HTTP/1.1 200"), "{index}");
    assert!(index.contains("List users"), "{index}");
    assert!(index.contains("EventSource(\"/__rustman/reload\")"));
    let page = get(&addr, "/endpoints/users-list.html");
    assert!(page.contains("Bearer {{token}}"), "{page}");
    assert!(!page.contains(TOKEN));
    child.kill().unwrap();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("docs: built "), "{stderr}");
    assert!(!stderr.contains(TOKEN), "{stderr}");
    assert!(!dir.path().join("site").exists(), "serve writes no site/");
}

#[test]
fn a_busy_port_exits_2_naming_the_port() {
    let dir = workspace();
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port().to_string();
    let output = rustman(dir.path())
        .args(["docs", "serve", "--port", &port])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "docs: cannot bind 127.0.0.1:{port}: address in use"
        )),
        "{stderr}"
    );
}
