//! 下游 Client API Key 的准入策略
//!
//! Client API Key 在请求开始时冻结分组成员范围及其模型策略

mod client_version;
mod peer_sharing;

pub use peer_sharing::{ClientPeerSharing, ClientPeerSharingError};

pub use client_version::{
    ClientVersionRejection, CodexClientKind, CodexClientMinVersions, CodexClientVersion,
};

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::account::{FastMode, OpaqueProviderData, scope::FrozenAccountScope};
use crate::identity::ProviderKind;
use crate::validation::{IdentifierError, PolicyError, validate_text};

/// `client_api_keys.id` 的核心值对象
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientApiKeyId(String);

impl ClientApiKeyId {
    /// 校验并创建 Key ID
    ///
    /// # Errors
    ///
    /// ID 为空、过长或包含控制字符时返回错误
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

/// RuntimeSnapshot 中用于同步认证的明文 Client API Key
///
/// 数据库按产品约束明文保存；该值对象只负责阻止 `Debug`/日志意外输出
#[derive(Clone, PartialEq, Eq)]
pub struct PlaintextClientApiKey(String);

impl PlaintextClientApiKey {
    /// 校验并创建明文 Key
    ///
    /// # Errors
    ///
    /// Key 为空或无法作为 HTTP Bearer 值发送时返回错误
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        Self::validate(&value)?;
        Ok(Self(value))
    }

    /// 迁入的 Key 不限定前缀或长度；保持原值，仅校验 HTTP 可传输的非空可见 ASCII
    ///
    /// # Errors
    ///
    /// Key 为空或包含空白、控制字符、非 ASCII 字符时返回错误
    pub fn validate(value: &str) -> Result<(), IdentifierError> {
        if value.is_empty() {
            return Err(IdentifierError::Empty);
        }
        if !value.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(IdentifierError::InvalidFormat);
        }
        Ok(())
    }

    /// 仅借给同步认证器做常量时间比较
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

/// 密钥可用的绝对时间范围，包含起点、不包含终点
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientKeyValidity {
    pub starts_at: Option<chrono::DateTime<chrono::Utc>>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ClientKeyValidityStatus {
    Scheduled,
    Active,
    Expired,
}

impl ClientKeyValidity {
    #[must_use]
    pub fn is_valid(self) -> bool {
        match (self.starts_at, self.expires_at) {
            (None, None) => true,
            (Some(start), Some(end)) => start < end,
            _ => false,
        }
    }

    #[must_use]
    pub fn status_at(self, now: chrono::DateTime<chrono::Utc>) -> ClientKeyValidityStatus {
        if self.expires_at.is_some_and(|end| now >= end) {
            ClientKeyValidityStatus::Expired
        } else if self.starts_at.is_some_and(|start| now < start) {
            ClientKeyValidityStatus::Scheduled
        } else {
            ClientKeyValidityStatus::Active
        }
    }

    #[must_use]
    pub fn is_active(self) -> bool {
        self.status_at(chrono::Utc::now()) == ClientKeyValidityStatus::Active
    }
}

/// 零表示对应维度不额外限制
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimits {
    pub max_concurrency: u64,
    pub requests_per_minute: u64,
}

impl RateLimits {
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            max_concurrency: 0,
            requests_per_minute: 0,
        }
    }
}

/// Key 自身的设置默认值；请求派生策略保留本值，不能把宿主默认或插件覆盖写回
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSettings {
    pub request_profiles: BTreeMap<ProviderKind, OpaqueProviderData>,
    pub fast_mode: FastMode,
    pub limits: RateLimits,
}

/// 从 `client_api_keys` 冻结的公开准入事实
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientPolicy {
    validity: ClientKeyValidity,
    peer_sharing: ClientPeerSharing,
    name_snapshot: Option<String>,
    defaults: Arc<ClientSettings>,
    key_id: ClientApiKeyId,
    plaintext_key: PlaintextClientApiKey,
    account_scope: Arc<FrozenAccountScope>,
    enabled: bool,
    limits: RateLimits,
}

impl ClientPolicy {
    #[must_use]
    pub fn defaults(&self) -> &Arc<ClientSettings> {
        &self.defaults
    }

    pub(crate) fn with_settings(
        mut self,
        profiles: &BTreeMap<ProviderKind, OpaqueProviderData>,
        fast_mode: FastMode,
        limits: RateLimits,
    ) -> Self {
        if self.account_scope.request_profiles() != profiles
            || self.account_scope.fast_mode() != fast_mode
        {
            self.account_scope = Arc::new(
                self.account_scope
                    .as_ref()
                    .clone()
                    .with_request_profiles(profiles.clone())
                    .with_fast_mode(fast_mode),
            );
        }
        self.limits = limits;
        self
    }

    #[must_use]
    pub fn new(
        key_id: ClientApiKeyId,
        plaintext_key: PlaintextClientApiKey,
        account_scope: Arc<FrozenAccountScope>,
        enabled: bool,
        limits: RateLimits,
    ) -> Self {
        Self {
            validity: ClientKeyValidity::default(),
            peer_sharing: ClientPeerSharing::default(),
            name_snapshot: None,
            defaults: Arc::new(ClientSettings {
                request_profiles: account_scope.request_profiles().clone(),
                fast_mode: account_scope.fast_mode(),
                limits,
            }),
            key_id,
            plaintext_key,
            account_scope,
            enabled,
            limits,
        }
    }

    #[must_use]
    pub const fn with_validity(mut self, validity: ClientKeyValidity) -> Self {
        self.validity = validity;
        self
    }

    #[must_use]
    pub fn with_name_snapshot(mut self, name: Option<String>) -> Self {
        self.name_snapshot = name;
        self
    }

    #[must_use]
    pub fn with_peer_sharing(mut self, sharing: ClientPeerSharing) -> Self {
        if sharing.enabled {
            self.account_scope = Arc::new(self.account_scope.as_ref().clone().requiring_groups());
        }
        self.peer_sharing = sharing;
        self
    }

    #[must_use]
    pub const fn peer_sharing(&self) -> ClientPeerSharing {
        self.peer_sharing
    }

    #[must_use]
    pub fn name_snapshot(&self) -> Option<&str> {
        self.name_snapshot.as_deref()
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
    pub const fn account_scope(&self) -> &Arc<FrozenAccountScope> {
        &self.account_scope
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub const fn limits(&self) -> RateLimits {
        self.limits
    }

    /// 每次准入按当前时间检查冻结的有效期
    ///
    /// # Errors
    ///
    /// Key 已禁用或不在有效期内时返回拒绝原因
    pub fn authorize(&self) -> Result<(), PolicyError> {
        if !self.enabled {
            Err(PolicyError::Denied {
                reason: "client API key is disabled",
            })
        } else if self.validity.is_active() {
            Ok(())
        } else {
            Err(PolicyError::Denied {
                reason: "client API key is outside its validity period",
            })
        }
    }
}
