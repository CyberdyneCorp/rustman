//! Compiles the bundled `google/rpc` protos into a descriptor set (design
//! D8), embedded with `include_bytes!` to decode `grpc-status-details-bin`.
//! protox is pure Rust: no `protoc` is needed.

use std::path::PathBuf;

use prost::Message as _;

fn main() {
    let protos = PathBuf::from("protos");
    println!("cargo:rerun-if-changed=protos");
    let files = ["google/rpc/status.proto", "google/rpc/error_details.proto"];
    let set = protox::compile(files, [&protos]).expect("the bundled google/rpc protos compile");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(out.join("google_rpc.bin"), set.encode_to_vec()).expect("OUT_DIR is writable");
}
