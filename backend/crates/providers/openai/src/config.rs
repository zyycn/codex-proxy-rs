//! OpenAI Provider 启动配置与 Codex Desktop 请求画像校验。

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use url::{Host, Url};

use crate::credential::CodexQuotaRefreshPolicy;
use crate::transport::profile::{CodexWireProfile, CodexWireProfileState};
use crate::transport::session::{CodexSessionIdentity, CodexSessionIdentityError};
use crate::transport::websocket::CodexWebSocketPoolConfig;
use crate::{
    OFFICIAL_CODEX_BASE_URL,
    credential::token_client::{
        OFFICIAL_CODEX_OAUTH_CLIENT_ID, OFFICIAL_CODEX_TOKEN_ENDPOINT, TokenClientConfig,
    },
};

/// OpenAI Provider 唯一启动配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpenAiConfig {
    #[serde(default)]
    pub api: CodexApiConfig,
    #[serde(default)]
    pub ws_pool: CodexWebSocketPoolSettings,
    #[serde(default)]
    pub quota: CodexQuotaSettings,
    #[serde(default)]
    pub auth: CodexAuthSettings,
    pub wire_profile: CodexWireProfileConfig,
    #[serde(skip)]
    identity_secret_path: PathBuf,
}

impl OpenAiConfig {
    /// 校验 Provider-owned 字段，并定位旧版会话锚点持续使用的身份密钥。
    pub fn resolve_and_validate(&mut self, source_dir: &Path) -> Result<(), OpenAiConfigError> {
        self.api.validate()?;
        self.ws_pool.validate()?;
        self.quota.validate()?;
        self.auth.validate()?;
        self.wire_profile.validate()?;
        self.identity_secret_path = source_dir
            .parent()
            .unwrap_or(source_dir)
            .join(".runtime/data/identity_hmac_secret");
        Ok(())
    }

    #[must_use]
    pub fn wire_profile_state(&self) -> CodexWireProfileState {
        CodexWireProfileState::new(self.wire_profile.clone().into())
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
    pub const fn quota_skip_exhausted(&self) -> bool {
        self.quota.skip_exhausted
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
            wire_profile: CodexWireProfileConfig::default(),
            identity_secret_path: PathBuf::from(".runtime/data/identity_hmac_secret"),
        }
    }
}

/// Codex 上游 API 的 Provider-owned 地址配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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
        let url = Url::parse(&self.base_url)
            .map_err(|_| OpenAiConfigError::InvalidField("openai.api.base_url"))?;
        // 上游地址只接受 https；明文 http 仅放行本机回环（本地联调），
        // 避免把上游指向内网明文服务。凭据/查询串会污染端点拼接，一并拒绝。
        let is_loopback_host = match url.host() {
            Some(Host::Domain("localhost")) => true,
            Some(Host::Ipv4(address)) => address.is_loopback(),
            Some(Host::Ipv6(address)) => address.is_loopback(),
            Some(Host::Domain(_)) | None => false,
        };
        let is_loopback_http = url.scheme() == "http" && is_loopback_host;
        let is_secure_public = url.scheme() == "https" && url.host_str().is_some();
        if !(is_loopback_http || is_secure_public)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(OpenAiConfigError::InvalidField("openai.api.base_url"));
        }
        Ok(())
    }
}

/// Codex Responses WebSocket pool 的 Provider-owned 启动设置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodexWebSocketPoolSettings {
    pub enabled: bool,
    pub max_age_ms: u64,
    pub max_per_account: usize,
    pub max_total: usize,
    pub max_connecting: usize,
    pub initial_event_timeout_ms: u64,
}

impl Default for CodexWebSocketPoolSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_age_ms: 55 * 60 * 1000,
            max_per_account: 8,
            max_total: 64,
            max_connecting: 8,
            initial_event_timeout_ms: 20_000,
        }
    }
}

impl CodexWebSocketPoolSettings {
    fn validate(&self) -> Result<(), OpenAiConfigError> {
        if self.max_age_ms == 0
            || self.max_per_account == 0
            || self.max_total == 0
            || self.max_connecting == 0
            || self.max_connecting > self.max_total
        {
            return Err(OpenAiConfigError::InvalidField("openai.ws_pool"));
        }
        Ok(())
    }

    fn pool_config(&self) -> CodexWebSocketPoolConfig {
        CodexWebSocketPoolConfig {
            enabled: self.enabled,
            max_age: Duration::from_millis(self.max_age_ms),
            max_per_account: self.max_per_account,
            max_total: self.max_total,
            max_connecting: self.max_connecting,
            initial_event_timeout: (self.initial_event_timeout_ms != 0)
                .then(|| Duration::from_millis(self.initial_event_timeout_ms)),
            ..CodexWebSocketPoolConfig::default()
        }
    }
}

/// OpenAI Provider 的额度刷新策略。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodexQuotaSettings {
    pub refresh_interval_minutes: u64,
    pub skip_exhausted: bool,
}

impl Default for CodexQuotaSettings {
    fn default() -> Self {
        Self {
            refresh_interval_minutes: 5,
            skip_exhausted: true,
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
#[serde(deny_unknown_fields)]
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

/// 经审计固定的 Codex Desktop 上游请求画像。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodexWireProfileConfig {
    pub originator: String,
    pub codex_version: String,
    pub desktop_version: String,
    pub desktop_build: String,
    pub os_type: String,
    pub os_version: String,
    pub arch: String,
    pub terminal: String,
    pub verified_at: DateTime<Utc>,
}

impl Default for CodexWireProfileConfig {
    fn default() -> Self {
        Self {
            originator: "Codex Desktop".to_owned(),
            codex_version: "0.145.0".to_owned(),
            desktop_version: "26.721.31836".to_owned(),
            desktop_build: "5828".to_owned(),
            os_type: "Mac OS".to_owned(),
            os_version: "15.7.1".to_owned(),
            arch: "arm64".to_owned(),
            terminal: "unknown".to_owned(),
            verified_at: Utc::now(),
        }
    }
}

impl CodexWireProfileConfig {
    fn validate(&self) -> Result<(), OpenAiConfigError> {
        for (field, value) in [
            ("openai.wire_profile.originator", self.originator.as_str()),
            (
                "openai.wire_profile.codex_version",
                self.codex_version.as_str(),
            ),
            (
                "openai.wire_profile.desktop_version",
                self.desktop_version.as_str(),
            ),
            (
                "openai.wire_profile.desktop_build",
                self.desktop_build.as_str(),
            ),
            ("openai.wire_profile.os_type", self.os_type.as_str()),
            ("openai.wire_profile.os_version", self.os_version.as_str()),
            ("openai.wire_profile.arch", self.arch.as_str()),
            ("openai.wire_profile.terminal", self.terminal.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(OpenAiConfigError::InvalidField(field));
            }
        }
        if semver::Version::parse(&self.codex_version).is_err() {
            return Err(OpenAiConfigError::InvalidField(
                "openai.wire_profile.codex_version",
            ));
        }
        if !numeric_dotted_version(&self.desktop_version) {
            return Err(OpenAiConfigError::InvalidField(
                "openai.wire_profile.desktop_version",
            ));
        }
        if !self.desktop_build.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(OpenAiConfigError::InvalidField(
                "openai.wire_profile.desktop_build",
            ));
        }
        Ok(())
    }
}

impl From<CodexWireProfileConfig> for CodexWireProfile {
    fn from(value: CodexWireProfileConfig) -> Self {
        Self {
            originator: value.originator,
            codex_version: value.codex_version,
            desktop_version: value.desktop_version,
            desktop_build: value.desktop_build,
            os_type: value.os_type,
            os_version: value.os_version,
            arch: value.arch,
            terminal: value.terminal,
            verified_at: value.verified_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenAiConfigError {
    #[error("OpenAI configuration field is invalid: {0}")]
    InvalidField(&'static str),
}

fn numeric_dotted_version(value: &str) -> bool {
    let mut parts = value.split('.');
    let valid_parts = parts
        .by_ref()
        .filter(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        .count();
    valid_parts >= 2 && valid_parts == value.split('.').count()
}
