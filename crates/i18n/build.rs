#![forbid(unsafe_code)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../locales/nl");
    let dir = dir
        .canonicalize()
        .unwrap_or_else(|error| panic!("cannot find {}: {error}", dir.display()));
    println!("cargo::rerun-if-changed={}", dir.display());

    let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", dir.display()))
        .map(|entry| entry.expect("readable directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ftl"))
        .map(|path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_else(|| panic!("non-UTF-8 file name: {}", path.display()))
                .to_owned();
            (name, path)
        })
        .collect();
    files.sort();

    let mut generated = String::from("pub const DUTCH: &[(&str, &str)] = &[\n");
    for (name, path) in &files {
        println!("cargo::rerun-if-changed={}", path.display());
        let path = path
            .to_str()
            .unwrap_or_else(|| panic!("non-UTF-8 path: {}", path.display()));
        writeln!(generated, "    ({name:?}, include_str!({path:?})),").expect("write to String");
    }
    generated.push_str("];\n");

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"))
        .join("dutch_resources.rs");
    std::fs::write(&out, generated)
        .unwrap_or_else(|error| panic!("cannot write {}: {error}", out.display()));
}
