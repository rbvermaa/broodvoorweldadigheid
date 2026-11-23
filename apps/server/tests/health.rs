#![forbid(unsafe_code)]

use std::net::TcpListener as StdTcpListener;
use std::process::Command;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use db::PgPool;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tower::ServiceExt;

fn closed_port() -> u16 {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    port
}

fn unreachable_pool() -> PgPool {
    let url = format!("postgres://postgres@127.0.0.1:{}/postgres", closed_port());
    db::connect_lazy(&url, Duration::from_millis(300)).expect("lazy pool")
}

async fn get_health(pool: PgPool) -> (StatusCode, Value) {
    let response = server::router(pool)
        .oneshot(
            Request::get("/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024).await.expect("body");
    (status, serde_json::from_slice(&body).expect("json body"))
}

async fn run_healthcheck(url: String) -> Option<i32> {
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_server"))
            .args(["healthcheck", "--url", &url])
            .env_remove("DATABASE_URL")
            .status()
            .expect("run server healthcheck")
            .code()
    })
    .await
    .expect("join")
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn health_is_ok_when_the_database_answers(pool: PgPool) {
    let (status, body) = get_health(pool).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"status": "ok", "db": "ok"}));
}

#[tokio::test]
async fn health_is_degraded_when_the_database_is_down() {
    let (status, body) = get_health(unreachable_pool()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, json!({"status": "degraded", "db": "down"}));
}

#[sqlx::test(migrator = "db::MIGRATOR")]
async fn healthcheck_exits_0_against_a_healthy_server(pool: PgPool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(server::serve(listener, pool, async {
        let _ = stopped.await;
    }));

    let code = run_healthcheck(format!("http://{addr}/health")).await;

    let _ = stop.send(());
    serving.await.expect("join").expect("serve");
    assert_eq!(code, Some(0));
}

#[tokio::test]
async fn healthcheck_exits_1_when_the_server_reports_degraded() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(server::serve(listener, unreachable_pool(), async {
        let _ = stopped.await;
    }));

    let code = run_healthcheck(format!("http://{addr}/health")).await;

    let _ = stop.send(());
    serving.await.expect("join").expect("serve");
    assert_eq!(code, Some(1));
}

#[tokio::test]
async fn healthcheck_exits_1_against_a_closed_port() {
    let code = run_healthcheck(format!("http://127.0.0.1:{}/health", closed_port())).await;
    assert_eq!(code, Some(1));
}
