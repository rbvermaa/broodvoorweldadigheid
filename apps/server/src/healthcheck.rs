use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const DEFAULT_URL: &str = "http://127.0.0.1:8080/health";

const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_STATUS_LINE: usize = 1024;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum HealthcheckError {
    #[error("{0:?} is not an http:// URL with a host")]
    InvalidUrl(String),
    #[error("request failed: {0}")]
    Io(String),
    #[error("no answer within {0:?}")]
    Timeout(Duration),
    #[error("unexpected response: {0:?}")]
    BadResponse(String),
    #[error("status {0}")]
    Unhealthy(u16),
}

#[derive(Debug, PartialEq, Eq)]
struct Target<'a> {
    authority: &'a str,
    path: &'a str,
}

fn parse_url(url: &str) -> Result<Target<'_>, HealthcheckError> {
    let invalid = || HealthcheckError::InvalidUrl(url.to_string());
    let rest = url.strip_prefix("http://").ok_or_else(invalid)?;
    let (authority, path) = match rest.find('/') {
        Some(index) => rest.split_at(index),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(invalid());
    }
    Ok(Target { authority, path })
}

fn with_default_port(authority: &str) -> String {
    let has_port = match authority.rfind(':') {
        Some(index) => !authority[index..].contains(']'),
        None => false,
    };
    if has_port {
        authority.to_string()
    } else {
        format!("{authority}:80")
    }
}

fn status_code(response: &str) -> Result<u16, HealthcheckError> {
    let status_line = response.lines().next().unwrap_or_default();
    let mut parts = status_line.split_whitespace();
    match (parts.next(), parts.next().map(str::parse::<u16>)) {
        (Some(version), Some(Ok(code))) if version.starts_with("HTTP/1.") => Ok(code),
        _ => Err(HealthcheckError::BadResponse(status_line.to_string())),
    }
}

async fn fetch_status(target: &Target<'_>) -> Result<u16, HealthcheckError> {
    let io = |error: std::io::Error| HealthcheckError::Io(error.to_string());
    let mut stream = TcpStream::connect(with_default_port(target.authority))
        .await
        .map_err(io)?;
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        target.path, target.authority
    );
    stream.write_all(request.as_bytes()).await.map_err(io)?;
    let mut response = Vec::new();
    let mut buffer = [0_u8; 256];
    while !response.windows(2).any(|pair| pair == b"\r\n") && response.len() < MAX_STATUS_LINE {
        let read = stream.read(&mut buffer).await.map_err(io)?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read]);
    }
    status_code(&String::from_utf8_lossy(&response))
}

pub async fn check(url: &str) -> Result<(), HealthcheckError> {
    check_within(url, TIMEOUT).await
}

async fn check_within(url: &str, timeout: Duration) -> Result<(), HealthcheckError> {
    let target = parse_url(url)?;
    let code = tokio::time::timeout(timeout, fetch_status(&target))
        .await
        .map_err(|_| HealthcheckError::Timeout(timeout))??;
    verdict(code)
}

fn verdict(code: u16) -> Result<(), HealthcheckError> {
    if code == 200 {
        Ok(())
    } else {
        Err(HealthcheckError::Unhealthy(code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_is_split_into_authority_and_path() {
        assert_eq!(
            parse_url("http://127.0.0.1:8080/health"),
            Ok(Target {
                authority: "127.0.0.1:8080",
                path: "/health"
            })
        );
        assert_eq!(
            parse_url("http://server"),
            Ok(Target {
                authority: "server",
                path: "/"
            })
        );
    }

    #[test]
    fn non_http_urls_are_refused() {
        assert!(parse_url("https://127.0.0.1/health").is_err());
        assert!(parse_url("127.0.0.1:8080/health").is_err());
        assert!(parse_url("http:///health").is_err());
    }

    #[test]
    fn port_80_is_added_when_missing() {
        assert_eq!(with_default_port("server"), "server:80");
        assert_eq!(with_default_port("server:8080"), "server:8080");
        assert_eq!(with_default_port("[::1]"), "[::1]:80");
        assert_eq!(with_default_port("[::1]:8080"), "[::1]:8080");
    }

    #[test]
    fn status_code_is_read_from_the_status_line() {
        assert_eq!(status_code("HTTP/1.1 200 OK\r\n\r\n"), Ok(200));
        assert_eq!(status_code("HTTP/1.1 503 Service Unavailable\r\n"), Ok(503));
        assert!(status_code("").is_err());
        assert!(status_code("SSH-2.0-OpenSSH\r\n").is_err());
    }

    #[test]
    fn status_line_must_start_with_http_1() {
        assert!(status_code("XTTP/1.1 200 OK\r\n").is_err());
        assert!(status_code("HTTP/2 200\r\n").is_err());
    }

    #[test]
    fn only_200_counts_as_healthy() {
        assert_eq!(verdict(200), Ok(()));
        assert_eq!(verdict(204), Err(HealthcheckError::Unhealthy(204)));
        assert_eq!(verdict(503), Err(HealthcheckError::Unhealthy(503)));
    }

    #[tokio::test]
    async fn silent_server_times_out() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!("http://{}/health", listener.local_addr().expect("addr"));
        let timeout = Duration::from_millis(200);
        assert_eq!(
            check_within(&url, timeout).await,
            Err(HealthcheckError::Timeout(timeout))
        );
        drop(listener);
    }

    #[tokio::test]
    async fn answer_is_read_without_waiting_for_the_connection_to_close() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!("http://{}/health", listener.local_addr().expect("addr"));
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0_u8; 512];
            let _ = socket.read(&mut request).await.expect("read");
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                .await
                .expect("write");
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        assert_eq!(check_within(&url, Duration::from_secs(1)).await, Ok(()));
        server.abort();
    }
}
