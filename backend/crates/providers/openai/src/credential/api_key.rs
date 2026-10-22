//! API Key 账号的凭据合同；地址与传输策略随凭据 revision 一起更新

use std::fmt;

use secrecy::SecretString;
use serde::{Deserialize, Serialize};

pub(super) use crate::CODEX_AUTHENTICATION_KIND_API_KEY;

use super::types::ResponsesTransport;

/// 可在管理端展示的上游设置，不包含密钥
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKeyConfiguration {
    pub base_url: String,
    #[serde(default)]
    pub fetch_upstream_quota: bool,
    #[serde(default)]
    pub transport: ResponsesTransport,
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
}

impl ApiKeyCredentialData {
    pub fn configuration(&self) -> ApiKeyConfiguration {
        ApiKeyConfiguration {
            base_url: self.base_url.clone(),
            fetch_upstream_quota: false,
            transport: self.transport,
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

// 查询开关作为 Provider 凭据元数据保存，保留已有公开凭据构造合同
pub(crate) fn quota_setting(
    credential: &gateway_core::account::PlaintextCredential,
) -> Result<bool, super::CodexCredentialDataError> {
    credential
        .expose_to_provider()
        .get("fetch_upstream_quota")
        .map(|value| {
            value
                .as_bool()
                .ok_or(super::CodexCredentialDataError::Invalid)
        })
        .transpose()
        .map(|value| value.unwrap_or(false))
}

pub(crate) fn with_quota_setting(
    credential: gateway_core::account::PlaintextCredential,
    enabled: bool,
) -> gateway_core::account::PlaintextCredential {
    let mut data = credential.into_inner();
    if enabled {
        data.insert(
            "fetch_upstream_quota".to_owned(),
            serde_json::Value::Bool(true),
        );
    } else {
        data.remove("fetch_upstream_quota");
    }
    gateway_core::account::PlaintextCredential::new(data)
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
