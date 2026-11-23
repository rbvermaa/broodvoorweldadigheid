#![forbid(unsafe_code)]

use std::process::Command;

#[test]
fn binary_exits_successfully() {
    let status = Command::new(env!("CARGO_BIN_EXE_cli"))
        .status()
        .expect("binary starts");
    assert!(status.success(), "{status}");
}
