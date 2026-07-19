//! 下游 Client API Key 的准入策略。
//!
//! Client API Key 绑定一个 Provider 平台；模型名称不参与改写或 allowlist 判断。

use std::fmt;

use crate::error::{IdentifierError, PolicyError, validate_text};
use crate::routing::ProviderKind;

/// `client_api_keys.id` 的核心值对象。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientApiKeyId(String);

impl ClientApiKeyId {
    /// 校验并创建 Key ID。
    ///
    /// # Errors
    ///
    /// ID 为空、过长或包含控制字符时返回错误。
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_text(&value, 128, false, None)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ClientApiKeyId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// RuntimeSnapshot 中用于同步认证的明文 Client API Key。
///
/// 数据库按产品约束明文保存；该值对象只负责阻止 `Debug`/日志意外输出。
#[derive(Clone, PartialEq, Eq)]
pub struct PlaintextClientApiKey(String);

impl PlaintextClientApiKey {
    /// 校验并创建明文 Key。
    ///
    /// # Errors
    ///
    /// Key 为空、过长或包含控制字符时返回错误。
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_text(&value, 512, false, None)?;
        Ok(Self(value))
    }

    /// 仅借给同步认证器做常量时间比较。
    #[must_use]
    pub fn expose_for_auth(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PlaintextClientApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PlaintextClientApiKey(<redacted>)")
    }
}

/// 零表示对应维度不额外限制。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RateLimits {
    pub max_concurrency: u64,
    pub requests_per_minute: u64,
    pub tokens_per_minute: u64,
}

impl RateLimits {
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            max_concurrency: 0,
            requests_per_minute: 0,
            tokens_per_minute: 0,
        }
    }
}

/// 单次准入需要冻结的计数单位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionUnits {
    pub requests: u64,
    pub estimated_tokens: u64,
}

impl AdmissionUnits {
    /// 每次模型调用必须消耗一次 request。
    ///
    /// # Errors
    ///
    /// `requests` 为零时返回错误。
    pub fn new(requests: u64, estimated_tokens: u64) -> Result<Self, PolicyError> {
        if requests == 0 {
            return Err(PolicyError::Denied {
                reason: "request units must be positive",
            });
        }
        Ok(Self {
            requests,
            estimated_tokens,
        })
    }
}

/// 从 `client_api_keys` 冻结的公开准入事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientPolicy {
    key_id: ClientApiKeyId,
    plaintext_key: PlaintextClientApiKey,
    provider_kind: ProviderKind,
    enabled: bool,
    limits: RateLimits,
}

impl ClientPolicy {
    #[must_use]
    pub const fn new(
        key_id: ClientApiKeyId,
        plaintext_key: PlaintextClientApiKey,
        provider_kind: ProviderKind,
        enabled: bool,
        limits: RateLimits,
    ) -> Self {
        Self {
            key_id,
            plaintext_key,
            provider_kind,
            enabled,
            limits,
        }
    }

    #[must_use]
    pub const fn key_id(&self) -> &ClientApiKeyId {
        &self.key_id
    }

    #[must_use]
    pub const fn plaintext_key(&self) -> &PlaintextClientApiKey {
        &self.plaintext_key
    }

    #[must_use]
    pub const fn provider_kind(&self) -> &ProviderKind {
        &self.provider_kind
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub const fn limits(&self) -> RateLimits {
        self.limits
    }

    /// 禁用的 Key 不接受新请求。
    ///
    /// # Errors
    ///
    /// Key 已禁用时返回稳定拒绝原因。
    pub fn authorize(&self) -> Result<(), PolicyError> {
        if self.enabled {
            Ok(())
        } else {
            Err(PolicyError::Denied {
                reason: "client API key is disabled",
            })
        }
    }
}
