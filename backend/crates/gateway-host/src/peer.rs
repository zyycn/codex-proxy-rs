//! 互联请求复用账号出口和证书策略，重定向不会携带密钥跨站点转发

use std::time::{Duration, SystemTime};

use futures::StreamExt as _;
use gateway_core::account::{ProviderAccount, UpstreamInfoFailure};
use secrecy::ExposeSecret as _;

use gateway_admin::ports::peer::{PeerConnection, PeerFetchFailure, PeerTransport};
use std::sync::Arc;

type ClientBuilder = dyn Fn(reqwest::ClientBuilder) -> Result<reqwest::Client, ()> + Send + Sync;

/// 组合根注入证书策略；出口、超时、重定向和响应上限由 Host 统一执行
pub struct HttpPeerTransport {
    build_client: Arc<ClientBuilder>,
}

impl HttpPeerTransport {
    pub fn with_client_builder<F, E>(build: F) -> Self
    where
        F: Fn(reqwest::ClientBuilder) -> Result<reqwest::Client, E> + Send + Sync + 'static,
    {
        Self {
            build_client: Arc::new(move |builder| build(builder).map_err(|_| ())),
        }
    }
}

use gateway_protocol::peer::MAX_RESPONSE_BYTES as BODY_LIMIT;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[async_trait::async_trait]
impl PeerTransport for HttpPeerTransport {
    async fn fetch(
        &self,
        account: &ProviderAccount,
        authentication: &PeerConnection,
    ) -> Result<Vec<u8>, PeerFetchFailure> {
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(REQUEST_TIMEOUT)
            .timeout(REQUEST_TIMEOUT);
        if let Some(proxy) = account.outbound_proxy() {
            builder = builder.proxy(
                reqwest::Proxy::all(proxy.expose_url())
                    .map_err(|_| UpstreamInfoFailure::Unavailable)?,
            );
        }
        let client = (self.build_client)(builder).map_err(|_| UpstreamInfoFailure::Unavailable)?;
        let response = client
            .get(format!(
                "{}/cpr/link",
                authentication.base_url.trim_end_matches('/')
            ))
            .bearer_auth(authentication.bearer_token.expose_secret())
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|_| UpstreamInfoFailure::Unavailable)?;
        if !response.status().is_success() {
            let kind = match response.status().as_u16() {
                401 | 403 => UpstreamInfoFailure::Unauthorized,
                404 | 405 | 501 => UpstreamInfoFailure::Unsupported,
                429 => UpstreamInfoFailure::RateLimited,
                _ => UpstreamInfoFailure::Unavailable,
            };
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| {
                    value
                        .parse::<u64>()
                        .ok()
                        .map(Duration::from_secs)
                        .or_else(|| {
                            chrono::DateTime::parse_from_rfc2822(value)
                                .ok()
                                .and_then(|time| {
                                    SystemTime::from(time)
                                        .duration_since(SystemTime::now())
                                        .ok()
                                })
                        })
                })
                .map(|value| value.min(Duration::from_secs(24 * 60 * 60)));
            return Err(PeerFetchFailure { kind, retry_after });
        }
        if response
            .content_length()
            .is_some_and(|length| length > BODY_LIMIT as u64)
        {
            return Err(UpstreamInfoFailure::InvalidResponse.into());
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| UpstreamInfoFailure::Unavailable)?;
            if bytes.len().saturating_add(chunk.len()) > BODY_LIMIT {
                return Err(UpstreamInfoFailure::InvalidResponse.into());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}
