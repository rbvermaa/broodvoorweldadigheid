//! The jobs runner that every server starts next to the web server (ADR 0011, ADR 0020).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use db::PgPool;
use domain::Clock;
use tokio::sync::watch;

const SETUP_RETRY: Duration = Duration::from_secs(5);

/// One shutdown signal for the web server and the jobs runner.
#[derive(Clone)]
pub struct Shutdown(watch::Receiver<bool>);

impl Shutdown {
    /// The receiving side, and the trigger that fires it once `signal` completes.
    pub fn on(
        signal: impl Future<Output = ()> + Send + 'static,
    ) -> (Self, impl Future<Output = ()>) {
        let (tx, rx) = watch::channel(false);
        let trigger = async move {
            signal.await;
            tx.send_replace(true);
        };
        (Self(rx), trigger)
    }

    /// Completes once shutdown has been asked for, also when it was asked before.
    pub fn wait(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut rx = self.0.clone();
        async move {
            if rx.wait_for(|stop| *stop).await.is_err() {
                tracing::warn!("shutdown signal dropped");
            }
        }
    }
}

/// `host-pid`: apalis needs a worker id that differs per running server.
pub fn worker_id() -> String {
    let host = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|host| host.trim().to_owned())
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| "server".to_owned());
    format!("{host}-{}", std::process::id())
}

/// Creates the apalis tables, retrying while the database is down, then runs the
/// bake day tick until `shutdown`. Returns at shutdown, or when the worker fails.
///
/// # Errors
/// [`jobs::Error`] when the worker stops with an error.
pub async fn run_jobs(
    pool: PgPool,
    clock: Arc<dyn Clock>,
    worker_id: String,
    shutdown: Shutdown,
) -> Result<(), jobs::Error> {
    loop {
        match jobs::setup(&pool).await {
            Ok(()) => break,
            Err(error) => {
                tracing::warn!(%error, "cannot set up the job queue; trying again");
                tokio::select! {
                    () = shutdown.wait() => return Ok(()),
                    () = tokio::time::sleep(SETUP_RETRY) => {}
                }
            }
        }
    }
    tracing::info!(worker = %worker_id, "jobs runner started");
    jobs::run(pool, clock, worker_id, shutdown.wait()).await
}
