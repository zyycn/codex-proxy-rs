//! xAI Provider 启动配置与 Grok CLI 请求画像校验。

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::{GrokOAuthConfig, XaiWireProfileState};

/// xAI Provider 唯一启动配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct XaiConfig {
    pub wire_profile: XaiWireProfileConfig,
}

impl XaiConfig {
    /// 校验 Provider-owned 字段；当前配置不含文件系统路径。
    pub fn resolve_and_validate(&mut self, _source_dir: &Path) -> Result<(), XaiConfigError> {
        self.wire_profile.validate()?;
        self.oauth_config().map(|_| ())
    }

    #[must_use]
    pub fn wire_profile_state(&self) -> XaiWireProfileState {
        XaiWireProfileState::new(self.wire_profile.clone())
    }

    pub(crate) fn oauth_config(&self) -> Result<GrokOAuthConfig, XaiConfigError> {
        GrokOAuthConfig::official().map_err(|_| XaiConfigError::InvalidProtocol)
    }
}

/// 经参考实现核验的 Grok CLI 请求画像。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct XaiWireProfileConfig {
    pub client_identifier: String,
    pub client_version: String,
    pub client_mode: String,
    pub target_os: String,
    pub target_arch: String,
    pub verified_at: DateTime<Utc>,
}

impl XaiWireProfileConfig {
    fn validate(&self) -> Result<(), XaiConfigError> {
        for (field, value) in [
            (
                "xai.wire_profile.client_identifier",
                self.client_identifier.as_str(),
            ),
            (
                "xai.wire_profile.client_version",
                self.client_version.as_str(),
            ),
            ("xai.wire_profile.client_mode", self.client_mode.as_str()),
            ("xai.wire_profile.target_os", self.target_os.as_str()),
            ("xai.wire_profile.target_arch", self.target_arch.as_str()),
        ] {
            if value.is_empty()
                || value.len() > 64
                || !value.bytes().all(|byte| byte.is_ascii_graphic())
            {
                return Err(XaiConfigError::InvalidField(field));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XaiConfigError {
    #[error("xAI configuration field is invalid: {0}")]
    InvalidField(&'static str),
    #[error("xAI official protocol configuration is invalid")]
    InvalidProtocol,
}
