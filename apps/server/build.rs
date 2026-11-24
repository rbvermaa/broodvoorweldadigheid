#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[path = "src/tokens.rs"]
mod tokens;

fn main() -> ExitCode {
    let source_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../design/tokens.toml");
    println!("cargo::rerun-if-changed={}", source_path.display());
    println!("cargo::rerun-if-changed=src/tokens.rs");
    let source = match std::fs::read_to_string(&source_path) {
        Ok(source) => source,
        Err(error) => {
            println!(
                "cargo::error=cannot read {}: {error}",
                source_path.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let css = match tokens::generate(&source) {
        Ok(css) => css,
        Err(problems) => {
            for problem in problems.lines() {
                println!("cargo::error=design/tokens.toml: {problem}");
            }
            return ExitCode::FAILURE;
        }
    };
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let target = out_dir.join("tokens.css");
    if let Err(error) = std::fs::write(&target, css) {
        println!("cargo::error=cannot write {}: {error}", target.display());
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
