//! 固定的官方 Grok Build inference instance 配置。

use gateway_core::routing::{ProviderInstance, ProviderInstanceId};
use url::Url;

pub const XAI_PROVIDER_NAME: &str = "xai";
pub const GROK_CLI_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";

const OFFICIAL_GROK_HOST: &str = "cli-chat-proxy.grok.com";
const OFFICIAL_GROK_BASE_PATH: &str = "/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokProviderTransport {
    HttpSse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrokProviderInstanceConfig {
    id: ProviderInstanceId,
    base_url: Url,
    responses_url: Url,
}

impl GrokProviderInstanceConfig {
    /// Provider instance 只持久化公共 endpoint；transport 由 adapter 固定。
    pub fn from_snapshot(instance: &ProviderInstance) -> Result<Self, GrokProviderConfigError> {
        if instance.provider().as_str() != XAI_PROVIDER_NAME {
            return Err(GrokProviderConfigError::ProviderMismatch);
        }
        let base_url = validate_official_base_url(instance.base_url())?;
        let mut responses_url = base_url.clone();
        responses_url.set_path("/v1/responses");
        Ok(Self {
            id: instance.id().clone(),
            base_url,
            responses_url,
        })
    }

    #[must_use]
    pub const fn id(&self) -> &ProviderInstanceId {
        &self.id
    }

    #[must_use]
    pub const fn base_url(&self) -> &Url {
        &self.base_url
    }

    #[must_use]
    pub const fn responses_url(&self) -> &Url {
        &self.responses_url
    }

    #[must_use]
    pub const fn transport(&self) -> GrokProviderTransport {
        GrokProviderTransport::HttpSse
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrokProviderConfigError {
    #[error("provider instance does not belong to Grok Build")]
    ProviderMismatch,
    #[error("Grok Build provider base URL is invalid")]
    InvalidBaseUrl,
    #[error("Grok Build provider base URL is not allowed")]
    UnsafeBaseUrl,
}

fn validate_official_base_url(value: &str) -> Result<Url, GrokProviderConfigError> {
    let mut url = Url::parse(value).map_err(|_| GrokProviderConfigError::InvalidBaseUrl)?;
    if url.scheme() != "https"
        || url.host_str() != Some(OFFICIAL_GROK_HOST)
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), OFFICIAL_GROK_BASE_PATH | "/v1/")
    {
        return Err(GrokProviderConfigError::UnsafeBaseUrl);
    }
    url.set_path(OFFICIAL_GROK_BASE_PATH);
    Ok(url)
}
