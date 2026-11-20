//! With the `raven` feature, checks that the Raven C library can be found
//! before the bindings try to link it, so a missing library fails with a
//! message that says what to do instead of a linker error
//! (add-raven-visualizer task 3.1).

use std::env;
use std::path::Path;

const HELP: &str = "\
rustman-viz links the Raven C library (https://github.com/oscar-soirey/Raven, v0.3.0).
Download the package for your platform from
  https://github.com/oscar-soirey/Raven/releases/tag/v0.3.0
(e.g. raven-macos-universal-static.tar.gz), extract it, and point RAVEN_LIB_DIR
at its lib folder; RAVEN_STATIC=1 links the static library:
  RAVEN_LIB_DIR=/path/to/raven/lib RAVEN_STATIC=1 cargo build --features raven";

fn main() {
    println!("cargo:rerun-if-env-changed=RAVEN_LIB_DIR");
    println!("cargo:rerun-if-env-changed=RAVEN_STATIC");
    if env::var_os("CARGO_FEATURE_RAVEN").is_none() {
        return;
    }
    let Some(dir) = env::var_os("RAVEN_LIB_DIR") else {
        panic!("RAVEN_LIB_DIR is not set.\n{HELP}");
    };
    let dir = Path::new(&dir);
    let names: &[&str] = if env::var("RAVEN_STATIC").is_ok_and(|v| v == "1") {
        &["libraven.a", "raven.lib"]
    } else {
        &["libraven.so", "libraven.dylib", "raven.lib", "libraven.a"]
    };
    if !names.iter().any(|name| dir.join(name).is_file()) {
        panic!(
            "RAVEN_LIB_DIR={} holds none of {}.\n{HELP}",
            dir.display(),
            names.join(", ")
        );
    }
}
