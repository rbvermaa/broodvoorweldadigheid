#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use e2e::Error;
use e2e::flows::{Ctx, Flow, run};
use e2e::mollie::MockMollie;
use i18n::Translator;
use tokio::net::TcpListener;

/// Runs every browser flow against a running server, each in a fresh Firefox session. The
/// server must use `MOLLIE_API_URL=http://<mollie-listen>/v2/`, an https `PUBLIC_URL`,
/// `COOKIE_SECURE=false`, `GALLERY=true`, and `DEV_STAFF_LOGIN=true` with a loopback
/// `BIND_ADDR` (the staff flows sign in through the dev login); the database must be
/// migrated. Each flow seeds its own data, so the binary can run again on the same database.
#[derive(Parser)]
#[command(name = "e2e")]
struct Args {
    #[arg(long, env = "E2E_SERVER_URL", default_value = "http://127.0.0.1:8080")]
    server_url: String,
    #[arg(
        long,
        env = "E2E_WEBDRIVER_URL",
        default_value = "http://127.0.0.1:4444"
    )]
    webdriver_url: String,
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    database_url: String,
    /// Where the mock Mollie listens; it must be free when the flows start.
    #[arg(long, env = "E2E_MOLLIE_LISTEN", default_value = "127.0.0.1:8090")]
    mollie_listen: String,
    /// axe-core's axe.min.js, from the flake (AXE_JS in the dev shell).
    #[arg(long, env = "AXE_JS")]
    axe_js: PathBuf,
    /// Seconds to wait for the server and the WebDriver to answer.
    #[arg(long, default_value_t = 120)]
    ready_timeout: u64,
}

async fn wait_until_up(url: &str, timeout: Duration) -> Result<(), Error> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match client.get(url).send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            _ if tokio::time::Instant::now() >= deadline => {
                return Err(format!("{url} did not answer within {timeout:?}").into());
            }
            _ => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
}

async fn setup(args: &Args) -> Result<Ctx, Error> {
    let timeout = Duration::from_secs(args.ready_timeout);
    let server = args.server_url.trim_end_matches('/').to_owned();
    let webdriver = args.webdriver_url.trim_end_matches('/').to_owned();
    wait_until_up(&format!("{server}/health"), timeout).await?;
    wait_until_up(&format!("{webdriver}/status"), timeout).await?;
    let listener = TcpListener::bind(&args.mollie_listen)
        .await
        .map_err(|err| format!("mock Mollie on {}: {err}", args.mollie_listen))?;
    let (mollie, _) = MockMollie::start(listener)?;
    Ok(Ctx {
        server,
        webdriver,
        pool: sqlx::PgPool::connect(&args.database_url).await?,
        mollie,
        axe_js: args.axe_js.clone(),
        i18n: Translator::dutch()?,
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let ctx = match setup(&args).await {
        Ok(ctx) => ctx,
        Err(err) => {
            eprintln!("e2e setup failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    let mut failed = 0;
    for flow in Flow::ALL {
        match run(&ctx, flow).await {
            Ok(()) => println!("PASS {}", flow.name()),
            Err(err) => {
                failed += 1;
                println!("FAIL {}: {err}", flow.name());
            }
        }
    }
    println!("{} passed, {failed} failed", Flow::ALL.len() - failed);
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
