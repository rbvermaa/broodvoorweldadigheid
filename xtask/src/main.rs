#![forbid(unsafe_code)]

mod compose_smoke;
mod coverage;
mod layers;
mod metadata;
mod model;
mod unsafe_forbid;

use std::process::ExitCode;

const USAGE: &str =
    "usage: cargo xtask <layers|unsafe-forbid|coverage <lcov-report>|compose-smoke [options]>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (task, result) = match args.as_slice() {
        [task] if task == "layers" => (
            task,
            metadata::load(None).map(|metadata| layers::check(&metadata::crates(&metadata))),
        ),
        [task] if task == "unsafe-forbid" => (
            task,
            metadata::load(None).and_then(|metadata| {
                let mut violations = unsafe_forbid::check(&metadata::root_files(&metadata)?);
                violations.extend(unsafe_forbid::check_manifests(&metadata::manifests(
                    &metadata,
                )?));
                Ok(violations)
            }),
        ),
        [task, report] if task == "coverage" => (
            task,
            std::fs::read_to_string(report)
                .map_err(|error| format!("cannot read {report}: {error}"))
                .and_then(|contents| coverage::check(&contents)),
        ),
        [task, rest @ ..] if task == "compose-smoke" => (task, compose_smoke::run(rest)),
        _ => {
            eprintln!("{USAGE}");
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
