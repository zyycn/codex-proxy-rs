//! xAI Provider 启动配置。

use std::path::Path;

use serde::Deserialize;

use crate::{GrokOAuthConfig, transport::GROK_CLIENT_VERSION};

/// xAI 的协议端点、client 与版本均为经过校验的官方固定事实。
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct XaiConfig {}

impl XaiConfig {
    /// 校验 Provider-owned 固定协议配置；当前没有相对路径字段。
    pub fn resolve_and_validate(&mut self, _source_dir: &Path) -> Result<(), XaiConfigError> {
        self.oauth_config().map(|_| ())
    }

    pub(crate) fn oauth_config(self) -> Result<GrokOAuthConfig, XaiConfigError> {
        GrokOAuthConfig::official(GROK_CLIENT_VERSION).map_err(|_| XaiConfigError::InvalidProtocol)
    }
}

/// xAI 固定协议配置无法构造。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum XaiConfigError {
    #[error("xAI official protocol configuration is invalid")]
    InvalidProtocol,
}
