#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let run_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default()
        .to_string();
    match cli::execute(cli::Cli::parse().command, &run_id).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
