#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::time::Duration;

#[cfg(feature = "ai")]
use ai as _;
use clap::{Parser, Subcommand};
use server::config::Config;
use server::healthcheck;
use tokio::net::TcpListener;

const DB_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Parser)]
#[command(name = "server")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web server (default)
    Serve,
    /// GET the health URL; exit 0 on 200, 1 otherwise
    Healthcheck {
        #[arg(long, default_value = healthcheck::DEFAULT_URL)]
        url: String,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match Cli::parse().command.unwrap_or(Command::Serve) {
        Command::Serve => serve().await,
        Command::Healthcheck { url } => match healthcheck::check(&url).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("unhealthy: {error}");
                ExitCode::FAILURE
            }
        },
    }
}

async fn serve() -> ExitCode {
    tracing_subscriber::fmt().json().init();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "server stopped");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    let pool = db::connect_lazy(&config.database_url, DB_ACQUIRE_TIMEOUT)?;
    let listener = TcpListener::bind(config.bind_addr).await?;
    tracing::info!(addr = %listener.local_addr()?, "listening");
    server::serve(listener, pool, server::shutdown_signal()).await?;
    Ok(())
}
