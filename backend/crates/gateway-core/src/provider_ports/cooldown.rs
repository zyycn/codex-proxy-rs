//! Provider 冷却与账号错误保护的运行状态端口

use super::ProviderStoreError;
/// 账号级 cooldown 的来源类别；调度状态统一按 `rate_limited` 处理
pub use crate::account::AccountCooldownKind as ProviderCooldownKind;
use crate::{
    account::{CredentialRevision, ProviderAccountId},
    routing::UpstreamModelId,
};
use futures::future::BoxFuture;
use std::time::SystemTime;

/// 可丢失的账号冷却事实，不进入持久状态；探测冻结到期后仍需确认恢复
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCooldown {
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
    until: SystemTime,
    kind: ProviderCooldownKind,
}

impl ProviderCooldown {
    #[must_use]
    pub const fn new(
        account_id: ProviderAccountId,
        credential_revision: CredentialRevision,
        until: SystemTime,
    ) -> Self {
        Self::new_with_kind(
            account_id,
            credential_revision,
            until,
            ProviderCooldownKind::RateLimit,
        )
    }

    #[must_use]
    pub const fn new_with_kind(
        account_id: ProviderAccountId,
        credential_revision: CredentialRevision,
        until: SystemTime,
        kind: ProviderCooldownKind,
    ) -> Self {
        Self {
            account_id,
            credential_revision,
            until,
            kind,
        }
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn credential_revision(&self) -> CredentialRevision {
        self.credential_revision
    }

    #[must_use]
    pub const fn until(&self) -> SystemTime {
        self.until
    }

    #[must_use]
    pub const fn scheduling_state(&self) -> crate::account::AccountCooldown {
        crate::account::AccountCooldown {
            until: self.until,
            kind: self.kind,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ProviderCooldownKind {
        self.kind
    }
}

/// 可丢失 cooldown 的细粒度作用域
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderCooldownScope {
    /// 只阻止同一账号调用指定上游模型
    UpstreamModel(UpstreamModelId),
}

impl ProviderCooldownScope {
    #[must_use]
    pub const fn upstream_model(model: UpstreamModelId) -> Self {
        Self::UpstreamModel(model)
    }

    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::UpstreamModel(_) => "model",
        }
    }

    #[must_use]
    pub fn value(&self) -> &str {
        match self {
            Self::UpstreamModel(model) => model.as_str(),
        }
    }
}

/// 不进入账号持久状态的账号+作用域 cooldown
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderScopedCooldown {
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
    scope: ProviderCooldownScope,
    until: SystemTime,
}

impl ProviderScopedCooldown {
    #[must_use]
    pub const fn new(
        account_id: ProviderAccountId,
        credential_revision: CredentialRevision,
        scope: ProviderCooldownScope,
        until: SystemTime,
    ) -> Self {
        Self {
            account_id,
            credential_revision,
            scope,
            until,
        }
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn credential_revision(&self) -> CredentialRevision {
        self.credential_revision
    }

    #[must_use]
    pub const fn scope(&self) -> &ProviderCooldownScope {
        &self.scope
    }

    #[must_use]
    pub const fn until(&self) -> SystemTime {
        self.until
    }
}

/// 单次实际上游尝试开始时捕获的反馈栅栏
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountProtectionGuard {
    pub account_id: ProviderAccountId,
    pub credential_revision: CredentialRevision,
    pub policy_generation: String,
    pub reset_generation: String,
    pub evidence_version: u64,
}

pub trait ProviderCooldownPort: Send + Sync {
    /// 由配置发布边界调用，只有保护配置变化才推进策略代次
    fn publish_protection_policy<'a>(
        &'a self,
        _policy: &'a crate::account::AccountErrorProtection,
        _revision: crate::routing::ConfigRevision,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }

    fn begin_protection<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _revision: CredentialRevision,
        _fingerprint: &'a str,
        _snapshot_revision: crate::routing::ConfigRevision,
    ) -> BoxFuture<'a, Result<Option<AccountProtectionGuard>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }

    /// 凭据提交后只使旧凭据保护反馈失效，保留账号与模型限流
    fn advance_protection_revision<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    /// 同一原子操作内递增窗口计数并冻结，活动冻结不被迟到反馈顺延
    fn record_protection_failure<'a>(
        &'a self,
        _guard: &'a AccountProtectionGuard,
        _policy: &'a crate::account::AccountErrorProtection,
        _matched: &'a [crate::account::AccountProtectionMatch],
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    /// 只清除开始时所见的未冻结证据，不解除冻结或清除较新的失败
    fn record_protection_success<'a>(
        &'a self,
        _guard: &'a AccountProtectionGuard,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn put_if_later(
        &self,
        cooldown: ProviderCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>>;

    fn read<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCooldown>, ProviderStoreError>>;

    fn clear<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>>;

    fn put_scoped_if_later(
        &self,
        cooldown: ProviderScopedCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>>;

    fn read_scoped<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        scope: &'a ProviderCooldownScope,
    ) -> BoxFuture<'a, Result<Option<ProviderScopedCooldown>, ProviderStoreError>>;

    fn clear_scoped<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        scope: &'a ProviderCooldownScope,
        through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>>;

    /// 删除账号时清除该账号全部 account/model scope cooldown key
    /// 生命周期与 credential revision 无关；不要求调用方持有 revision
    fn clear_all<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>>;

    /// 普通请求成功只清除临时限流，保护证据由带栅栏的专用反馈负责
    fn clear_after_success<'a>(
        &'a self,
        account_id: &'a ProviderAccountId,
        through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>>;
}
