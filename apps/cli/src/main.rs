#![forbid(unsafe_code)]

mod entrypoint;

use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "cli")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Container entrypoints: first-start setup, then exec the service
    #[command(subcommand)]
    Entrypoint(entrypoint::Service),
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Some(Command::Entrypoint(service)) => entrypoint::run(service),
        None => {
            let _ = Cli::command().print_help();
            ExitCode::SUCCESS
        }
    }
}
