//! Provider 会话亲和、绑定和排除事实

use super::{ProviderStoreError, ProviderStoreErrorKind};
use crate::{account::ProviderAccountId, identity::ProviderKind, policy::ClientApiKeyId};
use futures::future::BoxFuture;
use std::{collections::BTreeSet, fmt, time::Duration};

/// Provider 从原始会话锚点派生的不可逆亲和键
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderSessionAffinityKey(String);

impl ProviderSessionAffinityKey {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ProviderStoreError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            })
        {
            return Err(ProviderStoreError::new(
                ProviderStoreErrorKind::InvalidData,
                "validate provider session affinity key",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn expose_to_store(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ProviderSessionAffinityKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderSessionAffinityKey([OPAQUE])")
    }
}

/// 会话绑定快照；版本区分同一账号的不同认领，防止过期和 A → B → A 后的旧写入
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSessionBinding {
    account_id: ProviderAccountId,
    revision: String,
}

impl ProviderSessionBinding {
    pub fn new(
        account_id: ProviderAccountId,
        revision: String,
    ) -> Result<Self, ProviderStoreError> {
        if revision.len() != 32 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ProviderStoreError::new(
                ProviderStoreErrorKind::InvalidData,
                "decode provider session binding revision",
            ));
        }
        Ok(Self {
            account_id,
            revision,
        })
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }
}

/// 已观测请求关联及其账号迁移权限，由 Provider 解释协议后写入
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSessionAlias {
    pub session_key: ProviderSessionAffinityKey,
    pub root_session_key: Option<ProviderSessionAffinityKey>,
    pub follow_only: bool,
}

/// 发送前需要核对版本的会话绑定；首次认领的 expected 为空
#[derive(Clone, Copy)]
pub struct ProviderSessionClaim<'a> {
    pub key: &'a ProviderSessionAffinityKey,
    pub expected: Option<&'a ProviderSessionBinding>,
    pub account: &'a ProviderAccountId,
    pub ttl: Duration,
}

/// 仅成功的内置轮询推进游标；有会话的分配必须同时提交绑定
#[derive(Clone, Copy)]
pub enum ProviderAccountAdmission<'a> {
    Session(ProviderSessionClaim<'a>),
    RoundRobin {
        client_key: &'a ClientApiKeyId,
        cursor: u64,
        session: Option<ProviderSessionClaim<'a>>,
    },
}

impl<'a> ProviderAccountAdmission<'a> {
    #[must_use]
    pub const fn session(self) -> Option<ProviderSessionClaim<'a>> {
        match self {
            Self::Session(session) => Some(session),
            Self::RoundRobin { session, .. } => session,
        }
    }
}

/// 客户端作用域内的会话账号绑定，原始会话身份由 Provider 哈希后传入
pub trait ProviderSessionAffinityPort: Send + Sync {
    fn load<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
    ) -> BoxFuture<'a, Result<Option<ProviderSessionBinding>, ProviderStoreError>>;

    /// 读取本 Client Key 和 Provider 的下一轮位置，不推进游标
    fn round_robin_cursor<'a>(
        &'a self,
        provider: &'a ProviderKind,
        client_key: &'a ClientApiKeyId,
    ) -> BoxFuture<'a, Result<u64, ProviderStoreError>>;

    /// 原子核对并提交会话版本和轮询位置；冲突时均不写入，调用方释放租约重选
    fn admit_account<'a>(
        &'a self,
        provider: &'a ProviderKind,
        admission: ProviderAccountAdmission<'a>,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>>;

    /// 显式观测到的请求关联只指向会话键，不缓存账号，迁移后仍读取当前绑定
    fn load_alias<'a>(
        &'a self,
        provider: &'a ProviderKind,
        alias: &'a ProviderSessionAffinityKey,
    ) -> BoxFuture<'a, Result<Option<ProviderSessionAlias>, ProviderStoreError>>;

    /// 只允许首次关联或同目标续期，冲突时禁止把同一轮次改指其他会话
    fn bind_alias<'a>(
        &'a self,
        provider: &'a ProviderKind,
        alias: &'a ProviderSessionAffinityKey,
        session: &'a ProviderSessionAlias,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>>;
}

/// Provider 会话内已失败账号的可丢失排除集
///
/// Provider 自行派生会话键并决定何时写入或清理；Core 只承载调度所需的账号 ID
/// 与 compare-and-swap revision，不解释任一 Provider 协议字段
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSessionExclusions {
    excluded_accounts: BTreeSet<ProviderAccountId>,
    revision: String,
}

impl ProviderSessionExclusions {
    #[must_use]
    pub const fn new(excluded_accounts: BTreeSet<ProviderAccountId>, revision: String) -> Self {
        Self {
            excluded_accounts,
            revision,
        }
    }

    #[must_use]
    pub const fn excluded_accounts(&self) -> &BTreeSet<ProviderAccountId> {
        &self.excluded_accounts
    }

    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }
}

/// 可丢失的 Provider 会话级账号排除状态
///
/// 该端口不接收协议正文；Provider 只能以不可逆会话键、账号 ID 和固定 TTL 操作
pub trait ProviderSessionExclusionPort: Send + Sync {
    fn load<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
    ) -> BoxFuture<'a, Result<Option<ProviderSessionExclusions>, ProviderStoreError>>;

    fn record_failure<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
        account_id: &'a ProviderAccountId,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<ProviderSessionExclusions, ProviderStoreError>>;

    fn clear<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a ProviderSessionAffinityKey,
        expected_revision: &'a str,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>>;
}
