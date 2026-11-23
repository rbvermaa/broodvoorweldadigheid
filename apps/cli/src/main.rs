#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::Parser;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let command = match cli::Cli::parse().command {
        cli::Command::Entrypoint(service) => return cli::entrypoint::run(service),
        cli::Command::Backup(command) => command,
    };
    match cli::execute(command, jiff::Timestamp::now()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
