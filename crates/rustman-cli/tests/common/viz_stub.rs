//! A stub viz companion for tests without a libtest harness
//! (`harness = false`): the test executable is its own companion. Started
//! with [`STUB`] set, it answers `hello` with the views of that variable
//! and saves every line it receives to the file named by [`STUB_LOG`].
//! Otherwise [`main`] runs the tests in order and fails the process if one
//! panics.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Set (to a comma-separated view list, possibly empty) when the test
/// executable runs as the stub companion.
pub const STUB: &str = "RUSTMAN_TEST_VIZ_STUB_VIEWS";
/// Where the stub saves the lines it receives.
pub const STUB_LOG: &str = "RUSTMAN_TEST_VIZ_STUB_LOG";

/// A named test.
pub type Test = (&'static str, fn());

/// Be the stub when [`STUB`] is set; otherwise run `tests` (those whose
/// name contains the first non-flag argument, when one is given).
pub fn main(tests: &[Test]) {
    if let Some(views) = std::env::var_os(STUB) {
        stub(&views.to_string_lossy());
        return;
    }
    let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    let mut failed = Vec::new();
    for (name, test) in tests {
        if filter.as_deref().is_some_and(|f| !name.contains(f)) {
            continue;
        }
        let ok = std::panic::catch_unwind(test).is_ok();
        println!("test {name} ... {}", if ok { "ok" } else { "FAILED" });
        if !ok {
            failed.push(*name);
        }
    }
    if !failed.is_empty() {
        eprintln!("failed: {failed:?}");
        std::process::exit(101);
    }
}

/// The stub companion: `rustman-viz/1` for `--protocol-check`; otherwise
/// `hello_ack` with `views`, then every line received until stdin closes,
/// saved at once to the log (written beside it first, then renamed, so a
/// reader never sees half of it).
fn stub(views: &str) {
    if std::env::args().any(|a| a == "--protocol-check") {
        println!("rustman-viz/1");
        return;
    }
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let _hello = lines.next();
    let views: Vec<&str> = views.split(',').filter(|v| !v.is_empty()).collect();
    let ack =
        json!({"type": "hello_ack", "protocol": "rustman-viz/1", "viewer": "stub", "views": views});
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{ack}").unwrap();
    stdout.flush().unwrap();
    let received: Vec<String> = lines.map_while(Result::ok).collect();
    let log = PathBuf::from(std::env::var_os(STUB_LOG).unwrap());
    let partial = log.with_extension("partial");
    std::fs::write(&partial, received.join("\n")).unwrap();
    std::fs::rename(partial, log).unwrap();
}

/// The messages the stub saved to `log`, once it has (waiting up to 30 s).
pub fn saved(log: &Path) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !log.exists() {
        assert!(Instant::now() < deadline, "the stub never saved its log");
        std::thread::sleep(Duration::from_millis(20));
    }
    let text = std::fs::read_to_string(log).unwrap();
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect()
}
