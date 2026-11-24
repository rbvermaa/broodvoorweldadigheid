use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use auth::oidc::{HttpError, HttpFuture, HttpRequest, OidcHttp, error_chain, http};
use reqwest::{Certificate, Client, redirect};

const TIMEOUT: Duration = Duration::from_secs(10);
/// Discovery documents, key sets and token responses are a few kB.
const MAX_BODY: usize = 1024 * 1024;

/// Talks to the OIDC provider over rustls, trusting the system CAs plus `ca_file`.
/// The client is built on first use and again after a connection failure, so a CA file
/// written or replaced after start-up is picked up.
pub struct ReqwestHttp {
    ca_file: Option<PathBuf>,
    client: Arc<Mutex<Option<Client>>>,
}

impl ReqwestHttp {
    #[must_use]
    pub fn new(ca_file: Option<PathBuf>) -> Self {
        Self {
            ca_file,
            client: Arc::new(Mutex::new(None)),
        }
    }

    fn client(&self) -> Result<Client, HttpError> {
        let mut cached = self.client.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(client) = cached.as_ref() {
            return Ok(client.clone());
        }
        let client = build_client(self.ca_file.as_deref())?;
        *cached = Some(client.clone());
        Ok(client)
    }
}

/// # Errors
/// When the CA file cannot be read or holds no certificate, or no root is trusted at all.
pub fn build_client(ca_file: Option<&Path>) -> Result<Client, HttpError> {
    let mut roots: Vec<Certificate> = rustls_native_certs::load_native_certs()
        .certs
        .iter()
        .filter_map(|der| Certificate::from_der(der).ok())
        .collect();
    if let Some(path) = ca_file {
        let pem = std::fs::read(path)
            .map_err(|error| HttpError(format!("cannot read {}: {error}", path.display())))?;
        let extra = Certificate::from_pem_bundle(&pem)
            .map_err(|error| HttpError(format!("{}: {error}", path.display())))?;
        if extra.is_empty() {
            return Err(HttpError(format!(
                "{} holds no certificate",
                path.display()
            )));
        }
        roots.extend(extra);
    }
    if roots.is_empty() {
        return Err(HttpError("no trusted CA certificates".to_string()));
    }
    roots
        .into_iter()
        .fold(
            Client::builder(),
            reqwest::ClientBuilder::add_root_certificate,
        )
        .redirect(redirect::Policy::none())
        .timeout(TIMEOUT)
        .https_only(true)
        .build()
        .map_err(|error| HttpError(format!("cannot build the HTTP client: {error}")))
}

impl OidcHttp for ReqwestHttp {
    fn send(&self, request: HttpRequest) -> HttpFuture {
        let client = self.client();
        let cache = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client?;
            let request = reqwest::Request::try_from(request)
                .map_err(|error| HttpError(error.to_string()))?;
            let mut response = client.execute(request).await.map_err(|error| {
                if error.is_connect() {
                    *cache.lock().unwrap_or_else(PoisonError::into_inner) = None;
                }
                HttpError(error_chain(&error))
            })?;
            let mut builder = http::Response::builder().status(response.status());
            if let Some(headers) = builder.headers_mut() {
                headers.extend(response.headers().clone());
            }
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| HttpError(error_chain(&error)))?
            {
                if body.len() + chunk.len() > MAX_BODY {
                    return Err(HttpError("response body too large".to_string()));
                }
                body.extend_from_slice(&chunk);
            }
            builder
                .body(body)
                .map_err(|error| HttpError(error.to_string()))
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// A self-signed certificate made for these tests only.
    const TEST_CA: &str = "\
-----BEGIN CERTIFICATE-----
MIICqjCCAZICCQDV98p0nDIKnjANBgkqhkiG9w0BAQsFADAWMRQwEgYDVQQDDAti
dncgdGVzdCBDQTAgFw0yNjExMjQwNjMyMDZaGA8yMTI2MTAzMTA2MzIwNlowFjEU
MBIGA1UEAwwLYnZ3IHRlc3QgQ0EwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEK
AoIBAQC5iYsb/B88XGGKWnaEADbIcjcUkFATvVeklXcZYTx6iCt/SQoECO3iy/tp
IyeUjjeCqP/Xug9PlIe9XQW5xjBTJVoG7N100VsTymcFV0UWq0eVQk0mBkJ7fPrV
NPsEGMX5zqYM5i7wPMiem8qMYZd2fBUsVbI4hbxvAJZn357C9EarZeTqmTkOjXvF
ubwKFTe4S8AT+OJXxRBThq8KD1xGZlRUiiKqtwyfvk4Wl622Vp/RjBtLDVlknLdP
eOiYX5bF3ATS2SHw7Wt+dLC8tDA4KudTfaIzdDhNmZi8NESvHuj8g/F/Kygz4bQ1
D+fB0+RZtx51e8FZtTMvA3X0r5tTAgMBAAEwDQYJKoZIhvcNAQELBQADggEBAFIQ
GYpStXcIeaE4gWxS/b+4b0kwHYl4x0ntk4+5DvsQz2tXV6+6+4sTqgqEVx6OfYWG
bPAZQlov/jDHyRaS+1T/XlGJKg6Lua7rbdVR7UtR6UdCQFPUzd5OR/sy4aXtngoO
Eb7UUGpVrn9CccggBE9lQH5k3Q5oBRth2qOd9a9QVFR0NR4upImd3gKw/hPdbcml
jT7A6i+pLTFJtbuSjCbwV8F4oyVi1yma0Lr3SMb94pB5qCddBpv/nYNqz4B/QoOv
ljDxHtekSzEUyfGvB0pA4/AP0XwDD7WVQ9cJMYUudWiX+jNgPv4sdb82VF4AcjOu
udpcPAPQEx5DIy3YTpQ=
-----END CERTIFICATE-----
";

    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str, contents: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("server-oidc-http-{}-{name}", std::process::id()));
            std::fs::write(&path, contents).expect("write");
            Self(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn a_missing_or_empty_ca_file_is_an_error() {
        let missing = build_client(Some(Path::new("/nonexistent/ca.pem"))).expect_err("missing");
        assert!(missing.0.contains("cannot read"), "{missing}");

        let empty = TempFile::new("empty.pem", "");
        let error = build_client(Some(&empty.0)).expect_err("empty");
        assert!(error.0.contains("no certificate"), "{error}");
    }

    #[tokio::test]
    async fn the_client_is_built_once_and_refuses_plain_http() {
        let ca = TempFile::new("ca.pem", TEST_CA);
        let http = ReqwestHttp::new(Some(ca.0.clone()));
        let request = || {
            http::Request::get("http://127.0.0.1:1/")
                .body(Vec::new())
                .expect("request")
        };
        let error = http.send(request()).await.expect_err("http refused");
        assert!(error.0.contains("scheme"), "{error}");
        std::fs::remove_file(&ca.0).expect("remove CA");
        let error = http.send(request()).await.expect_err("http refused");
        assert!(error.0.contains("scheme"), "cached client: {error}");
    }
}
