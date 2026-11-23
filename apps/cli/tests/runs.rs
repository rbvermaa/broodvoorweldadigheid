#![forbid(unsafe_code)]

use std::process::Command;

#[test]
fn binary_prints_help() {
    let status = Command::new(env!("CARGO_BIN_EXE_cli"))
        .arg("--help")
        .status()
        .expect("binary starts");
    assert!(status.success(), "{status}");
}
