use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::KeyExtractor;
use tower_governor::{GovernorError, GovernorLayer};

use crate::pages;
use crate::state::AppState;

const FORGET_INTERVAL: Duration = Duration::from_secs(60);

/// The client's IP address: the TCP peer, or, when the peer is the trusted reverse proxy
/// (Caddy), the last X-Forwarded-For entry. Caddy appends the address it saw as the last
/// entry, so earlier entries are whatever the client sent and are never believed.
#[derive(Clone, Copy, Debug)]
pub struct ClientIp {
    pub trusted_proxy: Option<IpAddr>,
}

impl ClientIp {
    fn client<T>(&self, request: &Request<T>) -> Option<IpAddr> {
        let peer = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| addr.ip())?;
        if Some(peer) != self.trusted_proxy {
            return Some(peer);
        }
        let forwarded = request
            .headers()
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .last()
            .and_then(|entry| entry.trim().parse().ok());
        Some(forwarded.unwrap_or(peer))
    }
}

impl KeyExtractor for ClientIp {
    type Key = IpAddr;

    fn extract<T>(&self, request: &Request<T>) -> Result<IpAddr, GovernorError> {
        self.client(request)
            .ok_or(GovernorError::UnableToExtractKey)
    }
}

/// Limits `router` to `burst` requests per client at once, refilling one every `period`.
/// Over the limit the client gets the Dutch 429 page with a Retry-After header.
///
/// # Panics
/// When `period` is zero or `burst` is 0.
pub fn apply<S>(router: Router<S>, state: &AppState, period: Duration, burst: u32) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let config = GovernorConfigBuilder::default()
        .key_extractor(ClientIp {
            trusted_proxy: state.config.trusted_proxy,
        })
        .period(period)
        .burst_size(burst)
        .finish()
        .expect("period and burst are not zero");
    let limiter = Arc::downgrade(config.limiter());
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        // One entry per client address; drop those whose quota is full again.
        runtime.spawn(async move {
            let mut tick = tokio::time::interval(FORGET_INTERVAL);
            loop {
                tick.tick().await;
                let Some(limiter) = limiter.upgrade() else {
                    return;
                };
                limiter.retain_recent();
                limiter.shrink_to_fit();
            }
        });
    }
    let state = state.clone();
    router.layer(GovernorLayer::new(config).error_handler(move |error| error_page(&state, error)))
}

fn error_page(state: &AppState, error: GovernorError) -> Response {
    match error {
        GovernorError::TooManyRequests { headers, .. } => {
            let mut response = pages::error(state, StatusCode::TOO_MANY_REQUESTS);
            if let Some(headers) = headers {
                response.headers_mut().extend(headers);
            }
            response
        }
        GovernorError::UnableToExtractKey => {
            tracing::error!("rate limit: no client address; is ConnectInfo missing?");
            pages::error(state, StatusCode::INTERNAL_SERVER_ERROR)
        }
        GovernorError::Other { code, .. } => pages::error(state, code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(peer: &str, forwarded: &[&str]) -> Request<()> {
        let mut builder = Request::get("/");
        for value in forwarded {
            builder = builder.header("x-forwarded-for", *value);
        }
        let mut request = builder.body(()).expect("request");
        request.extensions_mut().insert(ConnectInfo(
            format!("{peer}:4000").parse::<SocketAddr>().expect("addr"),
        ));
        request
    }

    fn ip(raw: &str) -> IpAddr {
        raw.parse().expect("ip")
    }

    #[test]
    fn without_a_trusted_proxy_the_peer_counts_and_headers_are_ignored() {
        let extractor = ClientIp {
            trusted_proxy: None,
        };
        assert_eq!(
            extractor.client(&request("10.0.0.5", &["1.2.3.4"])),
            Some(ip("10.0.0.5"))
        );
    }

    #[test]
    fn behind_the_trusted_proxy_the_last_forwarded_entry_counts() {
        let extractor = ClientIp {
            trusted_proxy: Some(ip("10.89.0.10")),
        };
        assert_eq!(
            extractor.client(&request("10.89.0.10", &["6.6.6.6, 1.2.3.4"])),
            Some(ip("1.2.3.4"))
        );
        assert_eq!(
            extractor.client(&request("10.89.0.10", &["6.6.6.6", "1.2.3.4"])),
            Some(ip("1.2.3.4"))
        );
    }

    #[test]
    fn forwarded_header_from_another_peer_is_ignored() {
        let extractor = ClientIp {
            trusted_proxy: Some(ip("10.89.0.10")),
        };
        assert_eq!(
            extractor.client(&request("10.89.0.99", &["1.2.3.4"])),
            Some(ip("10.89.0.99"))
        );
    }

    #[test]
    fn proxy_without_a_usable_forwarded_header_counts_as_the_peer() {
        let extractor = ClientIp {
            trusted_proxy: Some(ip("10.89.0.10")),
        };
        assert_eq!(
            extractor.client(&request("10.89.0.10", &[])),
            Some(ip("10.89.0.10"))
        );
        assert_eq!(
            extractor.client(&request("10.89.0.10", &["1.2.3.4, garbage"])),
            Some(ip("10.89.0.10"))
        );
    }

    #[test]
    fn no_peer_address_means_no_key() {
        let extractor = ClientIp {
            trusted_proxy: None,
        };
        let request = Request::get("/").body(()).expect("request");
        assert!(matches!(
            extractor.extract(&request),
            Err(GovernorError::UnableToExtractKey)
        ));
    }
}
