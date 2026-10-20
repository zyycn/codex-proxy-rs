//! API Key 账号的凭据合同；地址与传输策略随凭据 revision 一起更新

use std::fmt;

use secrecy::SecretString;
use serde::{Deserialize, Serialize};

pub(super) use crate::CODEX_AUTHENTICATION_KIND_API_KEY;

use super::types::ResponsesTransport;

/// API Key 余额查询需显式选择，不根据推理地址探测上游类型
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyBalanceQuery {
    #[default]
    Disabled,
    Kimi,
    KimiGlobal,
    KimiCoding,
    KimiCodingGlobal,
    Deepseek,
    Sub2api,
}

impl ApiKeyBalanceQuery {
    pub(crate) const fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Kimi => "kimi",
            Self::KimiGlobal => "kimi_global",
            Self::KimiCoding => "kimi_coding",
            Self::KimiCodingGlobal => "kimi_coding_global",
            Self::Deepseek => "deepseek",
            Self::Sub2api => "sub2api",
        }
    }
}

/// 可在管理端展示的上游设置，不包含密钥
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKeyConfiguration {
    pub base_url: String,
    #[serde(default)]
    pub transport: ResponsesTransport,
    #[serde(default, skip_serializing_if = "ApiKeyBalanceQuery::is_disabled")]
    pub balance_query: ApiKeyBalanceQuery,
}

impl ApiKeyConfiguration {
    pub(crate) fn validate(&self) -> bool {
        self.base_url.len() <= 2048
            && !self.base_url.chars().any(char::is_control)
            && crate::transport::parse_upstream_base_url(&self.base_url).is_some()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKeyCredentialData {
    pub schema_version: u32,
    pub installation_id: String,
    pub api_key: String,
    pub base_url: String,
    #[serde(default)]
    pub transport: ResponsesTransport,
    #[serde(default, skip_serializing_if = "ApiKeyBalanceQuery::is_disabled")]
    pub balance_query: ApiKeyBalanceQuery,
}

impl ApiKeyCredentialData {
    pub fn configuration(&self) -> ApiKeyConfiguration {
        ApiKeyConfiguration {
            base_url: self.base_url.clone(),
            transport: self.transport,
            balance_query: self.balance_query,
        }
    }

    pub(crate) fn validate(&self) -> bool {
        self.schema_version == 1
            && self.configuration().validate()
            && !self.api_key.is_empty()
            && self.api_key.len() <= 16 * 1024
            && self.api_key.bytes().all(|byte| byte.is_ascii_graphic())
    }
}

impl fmt::Debug for ApiKeyCredentialData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApiKeyCredentialData")
            .field("schema_version", &self.schema_version)
            .field("configuration", &self.configuration())
            .field("api_key", &"<redacted>")
            .finish()
    }
}

pub struct ApiKeyAuthentication {
    pub configuration: ApiKeyConfiguration,
    pub secret: SecretString,
}

impl fmt::Debug for ApiKeyAuthentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApiKeyAuthentication")
            .field("configuration", &self.configuration)
            .field("secret", &"<redacted>")
            .finish()
    }
}
