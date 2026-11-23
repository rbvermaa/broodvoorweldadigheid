#![forbid(unsafe_code)]

use std::process::Command;

#[test]
fn postgres_entrypoint_fails_without_credentials_on_first_start() {
    let pgdata = std::env::temp_dir().join(format!("cli-entrypoint-it-{}", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_cli"))
        .args(["entrypoint", "postgres"])
        .env("PGDATA", &pgdata)
        .env_remove("POSTGRES_USER")
        .output()
        .expect("binary starts");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("POSTGRES_USER is not set"), "{stderr}");
}
