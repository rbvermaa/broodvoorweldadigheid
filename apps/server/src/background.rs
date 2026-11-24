//! The jobs runner that every server starts next to the web server (ADR 0011, ADR 0020).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use db::PgPool;
use domain::Clock;
use tokio::sync::watch;

const RETRY: Duration = Duration::from_secs(5);

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

/// Creates the apalis tables and runs the bake day tick until `shutdown`. Setup is raced
/// against `shutdown`; the worker stops by itself when it fires. After a failure, such as
/// a database restart, both start again, so scheduled work never stops while the server
/// runs. Each restart gets a fresh worker id: apalis refuses an id it still counts as live.
pub async fn run_jobs(pool: PgPool, clock: Arc<dyn Clock>, worker_id: String, shutdown: Shutdown) {
    for attempt in 1_u64.. {
        let id = if attempt == 1 {
            worker_id.clone()
        } else {
            format!("{worker_id}-{attempt}")
        };
        let setup = tokio::select! {
            () = shutdown.wait() => return,
            setup = jobs::setup(&pool) => setup,
        };
        let result = match setup {
            Ok(()) => {
                tracing::info!(worker = %id, "jobs runner started");
                jobs::run(pool.clone(), Arc::clone(&clock), id, shutdown.wait()).await
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => return,
            Err(error) => tracing::warn!(%error, attempt, "jobs runner failed; starting again"),
        }
        tokio::select! {
            () = shutdown.wait() => return,
            () = tokio::time::sleep(RETRY) => {}
        }
    }
}
