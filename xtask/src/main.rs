#![forbid(unsafe_code)]

mod layers;
mod metadata;
mod model;
mod unsafe_forbid;

use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask <layers|unsafe-forbid>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let task = match args.as_slice() {
        [task] => task.as_str(),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = match task {
        "layers" => {
            metadata::load(None).map(|metadata| layers::check(&metadata::crates(&metadata)))
        }
        "unsafe-forbid" => metadata::load(None)
            .and_then(|metadata| metadata::root_files(&metadata))
            .map(|roots| unsafe_forbid::check(&roots)),
        _ => {
            eprintln!("unknown task {task:?}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(violations) if violations.is_empty() => {
            println!("{task}: ok");
            ExitCode::SUCCESS
        }
        Ok(violations) => {
            for violation in &violations {
                eprintln!("{task}: {violation}");
            }
            eprintln!("{task}: {} violation(s)", violations.len());
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("{task}: {error}");
            ExitCode::FAILURE
        }
    }
}
