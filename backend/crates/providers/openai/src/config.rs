//! OpenAI Provider 启动配置；客户端身份由管理端设置持久化。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use url::Url;

use crate::credential::CodexQuotaRefreshPolicy;
use crate::transport::profile::CodexResidency;
use crate::transport::session::{CodexSessionIdentity, CodexSessionIdentityError};
use crate::transport::websocket::CodexWebSocketPoolConfig;
use crate::{
    OFFICIAL_CODEX_BASE_URL,
    credential::token_client::{
        OFFICIAL_CODEX_OAUTH_CLIENT_ID, OFFICIAL_CODEX_TOKEN_ENDPOINT, TokenClientConfig,
    },
};

/// 本服务的流式请求默认重试次数。
pub const DEFAULT_STREAM_MAX_RETRIES: u64 = 5;
/// 防止错误配置产生无界隐藏重放；与官方 Codex 的硬上限一致。
pub const MAX_STREAM_MAX_RETRIES: u64 = 100;

const fn default_stream_max_retries() -> u64 {
    DEFAULT_STREAM_MAX_RETRIES
}

/// OpenAI Provider 唯一启动配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct OpenAiConfig {
    #[serde(default)]
    pub api: CodexApiConfig,
    #[serde(default)]
    pub ws_pool: CodexWebSocketPoolSettings,
    #[serde(default)]
    pub quota: CodexQuotaSettings,
    #[serde(default)]
    pub auth: CodexAuthSettings,
    #[serde(default = "default_stream_max_retries")]
    pub stream_max_retries: u64,
    #[serde(default)]
    pub residency: Option<CodexResidency>,
    #[serde(skip)]
    identity_secret_path: PathBuf,
}

impl OpenAiConfig {
    /// 校验 Provider-owned 字段，并从统一运行数据目录定位会话身份密钥。
    pub fn resolve_and_validate(
        &mut self,
        runtime_data_dir: &Path,
    ) -> Result<(), OpenAiConfigError> {
        self.api.validate()?;
        self.ws_pool.validate()?;
        self.quota.validate()?;
        self.auth.validate()?;
        self.identity_secret_path = runtime_data_dir.join("identity_hmac_secret");
        Ok(())
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.api.base_url
    }

    #[must_use]
    pub fn websocket_pool_config(&self) -> CodexWebSocketPoolConfig {
        self.ws_pool.pool_config()
    }

    #[must_use]
    pub fn quota_refresh_policy(&self) -> CodexQuotaRefreshPolicy {
        self.quota.refresh_policy()
    }

    #[must_use]
    pub fn token_client_config(&self) -> TokenClientConfig {
        TokenClientConfig {
            client_id: self.auth.oauth_client_id.clone(),
            token_endpoint: self.auth.oauth_token_endpoint.clone(),
        }
    }

    #[must_use]
    pub fn oauth_client_id(&self) -> &str {
        &self.auth.oauth_client_id
    }

    #[must_use]
    pub const fn oauth_refresh_enabled(&self) -> bool {
        self.auth.refresh_enabled
    }

    /// 返回经官方同款硬上限约束后的上游流重试预算。
    #[must_use]
    pub fn stream_max_retries(&self) -> u32 {
        u32::try_from(self.stream_max_retries.min(MAX_STREAM_MAX_RETRIES))
            .unwrap_or(MAX_STREAM_MAX_RETRIES as u32)
    }

    pub(crate) fn session_identity(
        &self,
    ) -> Result<CodexSessionIdentity, CodexSessionIdentityError> {
        CodexSessionIdentity::load_or_create(&self.identity_secret_path)
    }
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            api: CodexApiConfig::default(),
            ws_pool: CodexWebSocketPoolSettings::default(),
            quota: CodexQuotaSettings::default(),
            auth: CodexAuthSettings::default(),
            stream_max_retries: DEFAULT_STREAM_MAX_RETRIES,
            residency: None,
            identity_secret_path: PathBuf::new(),
        }
    }
}

/// Codex 上游 API 的 Provider-owned 地址配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CodexApiConfig {
    pub base_url: String,
}

impl Default for CodexApiConfig {
    fn default() -> Self {
        Self {
            base_url: OFFICIAL_CODEX_BASE_URL.to_owned(),
        }
    }
}

impl CodexApiConfig {
    fn validate(&self) -> Result<(), OpenAiConfigError> {
        if !crate::transport::valid_upstream_base_url(&self.base_url) {
            return Err(OpenAiConfigError::InvalidField("openai.api.base_url"));
        }
        Ok(())
    }
}

/// Codex Responses WebSocket pool 的 Provider-owned 启动设置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CodexWebSocketPoolSettings {
    pub enabled: bool,
    pub max_age_ms: u64,
    pub max_connecting: usize,
    pub stream_idle_timeout_ms: u64,
}

impl Default for CodexWebSocketPoolSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_age_ms: 55 * 60 * 1000,
            max_connecting: 8,
            stream_idle_timeout_ms: 300_000,
        }
    }
}

impl CodexWebSocketPoolSettings {
    fn validate(&self) -> Result<(), OpenAiConfigError> {
        if self.max_age_ms == 0 || self.max_connecting == 0 {
            return Err(OpenAiConfigError::InvalidField("openai.ws_pool"));
        }
        Ok(())
    }

    fn pool_config(&self) -> CodexWebSocketPoolConfig {
        CodexWebSocketPoolConfig {
            enabled: self.enabled,
            max_age: Duration::from_millis(self.max_age_ms),
            max_connecting: self.max_connecting,
            stream_idle_timeout: (self.stream_idle_timeout_ms != 0)
                .then(|| Duration::from_millis(self.stream_idle_timeout_ms)),
            ..CodexWebSocketPoolConfig::default()
        }
    }
}

/// OpenAI Provider 的额度刷新策略。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CodexQuotaSettings {
    /// 保留模型目录的刷新周期；额度独立每 30 秒检查周期复核和 reset 到期条件。
    pub refresh_interval_minutes: u64,
}

impl Default for CodexQuotaSettings {
    fn default() -> Self {
        Self {
            refresh_interval_minutes: 15,
        }
    }
}

impl CodexQuotaSettings {
    fn validate(&self) -> Result<(), OpenAiConfigError> {
        if self.refresh_interval_minutes == 0 {
            return Err(OpenAiConfigError::InvalidField(
                "openai.quota.refresh_interval_minutes",
            ));
        }
        Ok(())
    }

    fn refresh_policy(&self) -> CodexQuotaRefreshPolicy {
        CodexQuotaRefreshPolicy::new(Duration::from_secs(
            self.refresh_interval_minutes.saturating_mul(60),
        ))
    }
}

/// OpenAI OAuth 的 Provider-owned 运行开关和端点。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CodexAuthSettings {
    pub refresh_enabled: bool,
    pub oauth_client_id: String,
    pub oauth_token_endpoint: String,
}

impl Default for CodexAuthSettings {
    fn default() -> Self {
        Self {
            refresh_enabled: true,
            oauth_client_id: OFFICIAL_CODEX_OAUTH_CLIENT_ID.to_owned(),
            oauth_token_endpoint: OFFICIAL_CODEX_TOKEN_ENDPOINT.to_owned(),
        }
    }
}

impl CodexAuthSettings {
    fn validate(&self) -> Result<(), OpenAiConfigError> {
        if self.oauth_client_id.trim().is_empty()
            || Url::parse(&self.oauth_token_endpoint)
                .ok()
                .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
                .is_none()
        {
            return Err(OpenAiConfigError::InvalidField("openai.auth"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenAiConfigError {
    #[error("OpenAI configuration field is invalid: {0}")]
    InvalidField(&'static str),
}
