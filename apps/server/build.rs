#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[path = "src/icon_set.rs"]
mod icon_set;
#[path = "src/tokens.rs"]
mod tokens;

fn main() -> ExitCode {
    let design = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../design");
    println!("cargo::rerun-if-changed=src/tokens.rs");
    println!("cargo::rerun-if-changed=src/icon_set.rs");
    println!("cargo::rerun-if-env-changed=LUCIDE_DIR");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));

    let tokens = read(&design.join("tokens.toml")).and_then(|source| {
        tokens::generate(&source).map_err(|problems| {
            problems
                .lines()
                .map(|problem| format!("design/tokens.toml: {problem}"))
                .collect::<Vec<_>>()
                .join("\n")
        })
    });
    let icons = read(&design.join("icons.toml")).and_then(|source| {
        let Some(lucide) = std::env::var_os("LUCIDE_DIR") else {
            return Err(
                "LUCIDE_DIR is not set; build inside `nix develop` or with `nix build`".to_string(),
            );
        };
        let lucide = PathBuf::from(lucide).join("icons");
        icon_set::generate(&source, |name| read(&lucide.join(format!("{name}.svg")))).map_err(
            |problems| {
                problems
                    .lines()
                    .map(|problem| format!("design/icons.toml: {problem}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
        )
    });

    let mut ok = true;
    for (result, file) in [(tokens, "tokens.css"), (icons, "icons.rs")] {
        let written = result.and_then(|content| {
            let target = out_dir.join(file);
            std::fs::write(&target, content)
                .map_err(|error| format!("cannot write {}: {error}", target.display()))
        });
        if let Err(problems) = written {
            for problem in problems.lines() {
                println!("cargo::error={problem}");
            }
            ok = false;
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn read(path: &Path) -> Result<String, String> {
    println!("cargo::rerun-if-changed={}", path.display());
    std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
}
