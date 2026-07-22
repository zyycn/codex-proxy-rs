//! xAI OAuth 账号输入、状态与明文 credential wire。

use std::fmt;

use chrono::{DateTime, Utc};
use gateway_core::engine::credential::{
    CredentialCasUpdate, CredentialRevision, LoadedCredential, ProviderAccountId,
    ProviderAccountUpdate,
};
use gateway_core::provider_ports::ProviderLeaseGuard;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::SecretValue;

/// 官方 xAI OAuth token pair；不接受 API Key。
pub struct GrokOAuthSecret {
    pub access_token: SecretValue,
    pub refresh_token: SecretValue,
    pub id_token: Option<SecretValue>,
    pub scope: String,
}

impl fmt::Debug for GrokOAuthSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokOAuthSecret")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("id_token", &self.id_token.as_ref().map(|_| "[REDACTED]"))
            .field("scope", &"[REDACTED]")
            .finish()
    }
}

/// 已由 OIDC 验证边界确认的 xAI 身份与 token 生命周期。
pub struct GrokAccountProfile {
    pub subject: String,
    pub email: Option<String>,
    pub upstream_account_id: Option<String>,
    pub plan_type: Option<String>,
    pub access_token_expires_at: DateTime<Utc>,
    /// Provider 明确返回时才保存；普通账号导出通常不携带 RT 过期时间。
    pub refresh_token_expires_at: Option<DateTime<Utc>>,
}

impl fmt::Debug for GrokAccountProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokAccountProfile")
            .field("subject", &"[REDACTED]")
            .field("email", &self.email.as_ref().map(|_| "[REDACTED]"))
            .field(
                "upstream_account_id",
                &self.upstream_account_id.as_ref().map(|_| "[REDACTED]"),
            )
            .field("plan_type", &self.plan_type)
            .field("access_token_expires_at", &self.access_token_expires_at)
            .field("refresh_token_expires_at", &self.refresh_token_expires_at)
            .finish()
    }
}

/// 与 `provider_accounts.availability` 一一对应的 xAI 状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrokCredentialAvailability {
    Unknown,
    Ready,
    Cooldown,
    QuotaExhausted,
    Expired,
    Banned,
    Invalid,
}

/// 创建一个明文 OAuth Provider account。
pub struct CreateGrokCredential {
    pub account_id: ProviderAccountId,
    pub name: String,
    pub secret: GrokOAuthSecret,
    pub account: GrokAccountProfile,
    pub next_refresh_at: DateTime<Utc>,
    pub enabled: bool,
    pub initial_availability: GrokCredentialAvailability,
    pub initial_availability_reason: Option<String>,
    pub initial_cooldown_until: Option<DateTime<Utc>>,
}

impl fmt::Debug for CreateGrokCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateGrokCredential")
            .field("account_id", &self.account_id)
            .field("name", &self.name)
            .field("secret", &"[REDACTED]")
            .field("account", &self.account)
            .field("next_refresh_at", &self.next_refresh_at)
            .field("enabled", &self.enabled)
            .field("initial_availability", &self.initial_availability)
            .finish_non_exhaustive()
    }
}

/// 用 credential revision CAS 轮换完整 OAuth token pair。
pub struct RotateGrokCredential {
    pub account_id: ProviderAccountId,
    pub expected_revision: CredentialRevision,
    pub secret: GrokOAuthSecret,
    pub verified_account: GrokAccountProfile,
    pub next_refresh_at: DateTime<Utc>,
}

/// App 已读取的当前账号与 xAI 已验证的新 OAuth 身份材料。
pub struct RotateManagedGrokCredential {
    pub current: LoadedCredential,
    pub secret: GrokOAuthSecret,
    pub verified_account: GrokAccountProfile,
    pub next_refresh_at: DateTime<Utc>,
}

impl fmt::Debug for RotateManagedGrokCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RotateManagedGrokCredential")
            .field("current", &self.current)
            .field("secret", &"[REDACTED]")
            .field("verified_account", &self.verified_account)
            .field("next_refresh_at", &self.next_refresh_at)
            .finish()
    }
}

/// xAI 验证后的管理员 rotation；App 只做 Core command 到原子事务的机械映射。
pub struct PreparedGrokCredentialRotation {
    pub profile: ProviderAccountUpdate,
    pub credential: CredentialCasUpdate,
    refresh_guards: Option<ProviderRefreshGuards>,
}

struct ProviderRefreshGuards {
    _capacity: Box<dyn ProviderLeaseGuard>,
    _account: Box<dyn ProviderLeaseGuard>,
}

impl fmt::Debug for PreparedGrokCredentialRotation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedGrokCredentialRotation")
            .field("profile", &self.profile)
            .field("credential", &self.credential)
            .field(
                "refresh_guards",
                &self.refresh_guards.as_ref().map(|_| "<held>"),
            )
            .finish()
    }
}

impl PreparedGrokCredentialRotation {
    pub(crate) fn new(profile: ProviderAccountUpdate, credential: CredentialCasUpdate) -> Self {
        Self {
            profile,
            credential,
            refresh_guards: None,
        }
    }

    pub(crate) fn with_refresh_guards(
        mut self,
        capacity: Box<dyn ProviderLeaseGuard>,
        account: Box<dyn ProviderLeaseGuard>,
    ) -> Self {
        self.refresh_guards = Some(ProviderRefreshGuards {
            _capacity: capacity,
            _account: account,
        });
        self
    }

    /// 将 command 与 lease 一起交给 App；返回的 guard 必须活到 CAS 提交结束。
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        ProviderAccountUpdate,
        CredentialCasUpdate,
        PreparedGrokCredentialRotationGuard,
    ) {
        (
            self.profile,
            self.credential,
            PreparedGrokCredentialRotationGuard(self.refresh_guards),
        )
    }
}

/// 手工刷新从 token exchange 到数据库 CAS 完成期间持有的 Redis lease。
pub struct PreparedGrokCredentialRotationGuard(Option<ProviderRefreshGuards>);

impl fmt::Debug for PreparedGrokCredentialRotationGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PreparedGrokCredentialRotationGuard")
            .field(&self.0.as_ref().map(|_| "<held>"))
            .finish()
    }
}

impl fmt::Debug for RotateGrokCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RotateGrokCredential")
            .field("account_id", &self.account_id)
            .field("expected_revision", &self.expected_revision)
            .field("secret", &"[REDACTED]")
            .field("verified_account", &self.verified_account)
            .field("next_refresh_at", &self.next_refresh_at)
            .finish()
    }
}

/// 用 credential revision fence 更新账号状态。
#[derive(Clone, Debug)]
pub struct UpdateGrokCredentialState {
    pub account_id: ProviderAccountId,
    pub expected_revision: CredentialRevision,
    pub availability: GrokCredentialAvailability,
    pub availability_reason: Option<String>,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
}

/// 不包含 secret 的写入结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrokCredentialRecord {
    pub account_id: ProviderAccountId,
    pub credential_revision: CredentialRevision,
}

#[derive(Serialize)]
pub(crate) struct GrokOAuthSecretWire<'a> {
    pub(crate) schema_version: u32,
    pub(crate) auth_method: &'static str,
    pub(crate) access_token: &'a str,
    pub(crate) refresh_token: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id_token: Option<&'a str>,
    pub(crate) scope: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) refresh_token_expires_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
pub(crate) struct OwnedGrokOAuthSecretWire {
    pub(crate) schema_version: u32,
    pub(crate) auth_method: String,
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
    pub(crate) id_token: Option<String>,
    pub(crate) scope: String,
    #[zeroize(skip)]
    pub(crate) refresh_token_expires_at: Option<DateTime<Utc>>,
}
