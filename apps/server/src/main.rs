#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "ai")]
use ai as _;
use clap::{Parser, Subcommand};
use i18n::Translator;
use server::AppState;
use server::background::{self, Shutdown};
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
    let payments = server::payment_provider::from_config(&config)?;
    let state = AppState::new(pool, Translator::dutch()?, config).with_payments(payments);
    let (shutdown, trigger) = Shutdown::on(server::shutdown_signal());
    tokio::spawn(trigger);
    let jobs = tokio::spawn(background::run_jobs(
        state.pool.clone(),
        Arc::clone(&state.clock),
        background::worker_id(),
        shutdown.clone(),
    ));
    tracing::info!(addr = %listener.local_addr()?, "listening");
    let served = server::serve(listener, state, shutdown.wait()).await;
    if let Err(error) = jobs.await {
        tracing::error!(%error, "jobs runner panicked");
    }
    served?;
    Ok(())
}
