//! 互联请求复用账号出口和证书策略，重定向不会携带密钥跨站点转发

use std::time::{Duration, SystemTime};

use futures::StreamExt as _;
use gateway_core::account::{ProviderAccount, UpstreamInfoFailure};
use secrecy::ExposeSecret as _;

use crate::{
    credential::ApiKeyAuthentication,
    transport::{client::build_account_http_client, endpoint_url},
};

use gateway_protocol::peer::MAX_RESPONSE_BYTES as BODY_LIMIT;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct FetchFailure {
    pub kind: UpstreamInfoFailure,
    pub retry_after: Option<Duration>,
}

impl From<UpstreamInfoFailure> for FetchFailure {
    fn from(kind: UpstreamInfoFailure) -> Self {
        Self {
            kind,
            retry_after: None,
        }
    }
}

pub(super) async fn fetch(
    account: &ProviderAccount,
    authentication: &ApiKeyAuthentication,
) -> Result<Vec<u8>, FetchFailure> {
    let client = build_account_http_client(account.id().as_str(), account.outbound_proxy())
        .map_err(|_| UpstreamInfoFailure::Unavailable)?;
    let response = client
        .get(endpoint_url(
            &authentication.configuration.base_url,
            "cpr/link",
        ))
        .bearer_auth(authentication.secret.expose_secret())
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
        return Err(FetchFailure { kind, retry_after });
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
