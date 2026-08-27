//! Codex Admin 输入的 Provider-owned 验证与明文 command preparation。
//!
//! 本模块不读写 Store；应用层负责把已验证的 Core command 映射到
//! 持久层的原子配置 revision + audit 事务。

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::SystemTime;

use chrono::{DateTime, FixedOffset, Utc};
use gateway_core::engine::credential::{
    AccountErrorReason, CredentialCasUpdate, CredentialRevision, CredentialState, LoadedCredential,
    NewProviderAccount, ProviderAccount, ProviderAccountId, ProviderAccountIdentity,
    ProviderAccountUpdate, QuotaState,
};
use gateway_core::provider_ports::{
    ProviderLeaseAcquisition, ProviderLeaseGuard, ProviderLeasePort, ProviderLeaseRequest,
    ProviderRefreshCapacityRequest, ProviderRefreshLeaseRequest, ProviderRuntimePolicyPort,
};
use gateway_core::routing::ProviderKind;
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use super::recovery_log::{CodexOAuthRecoveryOperation, record_oauth_recovery};
use super::security::CodexCredentialCodec;
use super::token_client::{RefreshFailure, TokenRefresher};
use super::types::{
    CODEX_AUTHENTICATION_KIND_OAUTH, CodexAccountProfile, CodexCredentialData,
    CodexCredentialPrincipal, CodexOAuthMetadata, CodexOAuthSecret, parse_access_token_expiration,
    parse_chatgpt_jwt_claims,
};

const PROVIDER_NAME: &str = "openai";
const MAX_BATCH: usize = 200;
const MAX_IMPORT_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

pub struct ImportCodexOAuthCredential {
    pub account_id: String,
    pub name: String,
    pub secret: CodexOAuthSecret,
    pub verified_account: CodexAccountProfile,
    pub next_refresh_at: Option<DateTime<Utc>>,
    pub enabled: bool,
}

/// OAuth credential 的最小创建输入。
///
/// metadata 来自 ID token/access token 的官方本地 payload 解析；不包含签名、
/// issuer、audience、nonce 或 token 验证。
pub(crate) struct UnresolvedCodexOAuthCredential {
    pub(crate) account_id: String,
    pub(crate) name: String,
    pub(crate) installation_id: String,
    pub(crate) secret: CodexOAuthSecret,
    pub(crate) metadata: CodexOAuthMetadata,
    pub(crate) access_token_expires_at: Option<DateTime<Utc>>,
    pub(crate) next_refresh_at: Option<DateTime<Utc>>,
    pub(crate) enabled: bool,
}

impl fmt::Debug for UnresolvedCodexOAuthCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UnresolvedCodexOAuthCredential")
            .field("account_id", &self.account_id)
            .field("name", &self.name)
            .field("installation_id", &"<pseudonymous>")
            .field("secret", &"<redacted>")
            .field("metadata", &self.metadata)
            .field("access_token_expires_at", &self.access_token_expires_at)
            .field("next_refresh_at", &self.next_refresh_at)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl std::fmt::Debug for ImportCodexOAuthCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImportCodexOAuthCredential")
            .field("account_id", &self.account_id)
            .field("name", &self.name)
            .field("secret", &"<redacted>")
            .field("verified_account", &self.verified_account)
            .field("next_refresh_at", &self.next_refresh_at)
            .field("enabled", &self.enabled)
            .finish()
    }
}

/// Provider-owned 文档归一后的唯一 Core 写入批次。
pub struct PreparedCodexAccountImport {
    accounts: Vec<NewProviderAccount>,
}

impl PreparedCodexAccountImport {
    #[must_use]
    pub fn accounts(&self) -> &[NewProviderAccount] {
        &self.accounts
    }

    #[must_use]
    pub fn into_accounts(self) -> Vec<NewProviderAccount> {
        self.accounts
    }
}

impl fmt::Debug for PreparedCodexAccountImport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedCodexAccountImport")
            .field("account_count", &self.accounts.len())
            .field("accounts", &"<redacted>")
            .finish()
    }
}

struct ParsedCodexImportAccount {
    name: Option<String>,
    email: Option<String>,
    authentication: ParsedCodexAuthentication,
}

struct ParsedCodexAuthentication {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

impl fmt::Debug for ParsedCodexAuthentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuth")
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("id_token", &self.id_token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl fmt::Debug for ParsedCodexImportAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParsedCodexImportAccount")
            .field("name", &self.name)
            .field("email", &self.email.as_ref().map(|_| "<redacted>"))
            .field("authentication", &self.authentication)
            .finish()
    }
}

/// Store 公共行事实与 Core 明文 credential 的导出输入。
///
/// 时间必须由 App 从 `provider_accounts` 原行机械传入；Provider 不伪造时间。
pub struct ExportManagedCodexCredential {
    pub current: LoadedCredential,
    pub added_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl fmt::Debug for ExportManagedCodexCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExportManagedCodexCredential")
            .field("account_id", &self.current.account.id())
            .field("credential", &"<redacted>")
            .field("added_at", &self.added_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

/// CPR canonical 账号导出文档；只允许显式序列化，Debug 永不输出 credential secret。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexCprExportDocument {
    source_format: &'static str,
    accounts: Vec<CodexCprOAuthExportAccount>,
}

impl CodexCprExportDocument {
    #[must_use]
    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    pub fn into_json(self) -> Result<Value, CodexCredentialAdminError> {
        serde_json::to_value(self).map_err(|_| CodexCredentialAdminError::InvalidCredential)
    }
}

impl fmt::Debug for CodexCprExportDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCprExportDocument")
            .field("source_format", &self.source_format)
            .field("account_count", &self.accounts.len())
            .field("accounts", &"<redacted>")
            .finish()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexCprExportCommon {
    id: String,
    email: Option<String>,
    account_id: Option<String>,
    user_id: Option<String>,
    label: Option<String>,
    plan_type: Option<String>,
    status: &'static str,
    added_at: String,
    updated_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexCprOAuthExportAccount {
    #[serde(flatten)]
    common: CodexCprExportCommon,
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    access_token_expires_at: Option<String>,
}

/// App 已从 Store 读取的当前账号、revision 与明文 Provider JSON。
pub struct RotateManagedCodexCredential {
    pub current: LoadedCredential,
    pub secret: CodexOAuthSecret,
    pub verified_account: CodexAccountProfile,
    pub next_refresh_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for RotateManagedCodexCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RotateManagedCodexCredential")
            .field("current", &self.current)
            .field("secret", &"<redacted>")
            .field("verified_account", &self.verified_account)
            .field("next_refresh_at", &self.next_refresh_at)
            .finish()
    }
}

/// Provider 验证后的 rotation；App 只做 Core -> Store command 的机械映射。
pub struct PreparedCodexCredentialRotation {
    pub profile: ProviderAccountUpdate,
    pub credential: CredentialCasUpdate,
    pub replacement_identity: Option<ProviderAccountIdentity>,
    refresh_guards: Option<ProviderRefreshGuards>,
}

struct ProviderRefreshGuards {
    _capacity: Box<dyn ProviderLeaseGuard>,
    _account: Box<dyn ProviderLeaseGuard>,
}

impl std::fmt::Debug for PreparedCodexCredentialRotation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedCodexCredentialRotation")
            .field("profile", &self.profile)
            .field("credential", &self.credential)
            .field("replacement_identity", &self.replacement_identity)
            .field(
                "refresh_guards",
                &self.refresh_guards.as_ref().map(|_| "<held>"),
            )
            .finish()
    }
}

impl PreparedCodexCredentialRotation {
    /// 将 command 与 lease 一起交给 App；App 必须让返回的 guard 活到 CAS 提交结束。
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        ProviderAccountUpdate,
        CredentialCasUpdate,
        Option<ProviderAccountIdentity>,
        PreparedCodexCredentialRotationGuard,
    ) {
        (
            self.profile,
            self.credential,
            self.replacement_identity,
            PreparedCodexCredentialRotationGuard(self.refresh_guards),
        )
    }
}

/// 手工刷新从 token exchange 到数据库 CAS 完成期间持有的 Redis lease。
pub struct PreparedCodexCredentialRotationGuard(Option<ProviderRefreshGuards>);

impl fmt::Debug for PreparedCodexCredentialRotationGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PreparedCodexCredentialRotationGuard")
            .field(&self.0.as_ref().map(|_| "<held>"))
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CodexCredentialAdminError {
    #[error("Codex account input is invalid")]
    InvalidInput,
    #[error("Codex credential JSON is invalid")]
    InvalidCredential,
    #[error("Codex account was not found")]
    NotFound,
    #[error("Codex account has no refresh token")]
    MissingRefreshToken,
    #[error("Codex refresh lease is unavailable")]
    RefreshLeaseUnavailable,
    #[error("Codex refresh token was rejected")]
    RefreshRejected { message: Option<String> },
    #[error("Codex account is banned")]
    AccountBanned { message: Option<String> },
    #[error("Codex refresh service is unavailable")]
    RefreshUnavailable,
    #[error("Codex refresh send state is ambiguous")]
    RefreshAmbiguous { message: Option<String> },
}

impl CodexCredentialAdminError {
    #[must_use]
    pub fn upstream_message(&self) -> Option<&str> {
        match self {
            Self::RefreshRejected { message }
            | Self::AccountBanned { message }
            | Self::RefreshAmbiguous { message } => message.as_deref(),
            Self::InvalidInput
            | Self::InvalidCredential
            | Self::NotFound
            | Self::MissingRefreshToken
            | Self::RefreshLeaseUnavailable
            | Self::RefreshUnavailable => None,
        }
    }
}

/// 无状态的 Codex Admin command preparer。
#[derive(Debug, Default, Clone, Copy)]
pub struct CodexCredentialAdmin;

impl CodexCredentialAdmin {
    pub fn prepare_import(
        &self,
        input: ImportCodexOAuthCredential,
    ) -> Result<NewProviderAccount, CodexCredentialAdminError> {
        let account_id = ProviderAccountId::new(input.account_id)
            .map_err(|_| CodexCredentialAdminError::InvalidInput)?;
        let provider = ProviderKind::new(PROVIDER_NAME)
            .map_err(|_| CodexCredentialAdminError::InvalidInput)?;
        if input.name.trim().is_empty() {
            return Err(CodexCredentialAdminError::InvalidInput);
        }
        let access_token_expires_at = optional_time(input.verified_account.access_token_expires_at);
        let revision =
            CredentialRevision::new(1).map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        let upstream_user_id = input.verified_account.chatgpt_user_id.clone();
        let credential =
            CodexCredentialCodec::encode_new(&input.secret, &input.verified_account, Vec::new())
                .map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        let account = ProviderAccount::new(
            account_id,
            provider,
            input.name,
            Some(upstream_user_id),
            CODEX_AUTHENTICATION_KIND_OAUTH.to_owned(),
            revision,
            access_token_expires_at,
        )
        .with_profile(
            input.verified_account.email,
            Some(input.verified_account.chatgpt_account_id),
            input.verified_account.plan_type,
        )
        .with_account_facts(
            input.enabled,
            CredentialState::Ready,
            QuotaState::unknown(),
            None,
            None,
        )
        .with_refresh_schedule(
            input.secret.refresh_token.is_some(),
            optional_time(input.next_refresh_at),
        );
        Ok(NewProviderAccount {
            account,
            credential,
        })
    }

    /// 为已取得 OAuth access token、但资料尚未补全的账号创建最小记录。
    ///
    /// ID token payload 只用于本地字段投影；创建路径绝不发起 usage/profile 请求。
    pub(crate) fn prepare_unresolved_oauth(
        &self,
        input: UnresolvedCodexOAuthCredential,
    ) -> Result<NewProviderAccount, CodexCredentialAdminError> {
        let account_id = ProviderAccountId::new(input.account_id)
            .map_err(|_| CodexCredentialAdminError::InvalidInput)?;
        let provider = ProviderKind::new(PROVIDER_NAME)
            .map_err(|_| CodexCredentialAdminError::InvalidInput)?;
        if input.name.trim().is_empty() {
            return Err(CodexCredentialAdminError::InvalidInput);
        }
        let revision =
            CredentialRevision::new(1).map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        let CodexOAuthMetadata {
            email,
            chatgpt_plan_type: plan_type,
            chatgpt_user_id: upstream_user_id,
            chatgpt_account_id: upstream_account_id,
        } = input.metadata;
        let has_upstream_user_id = upstream_user_id.is_some();
        let credential = CodexCredentialCodec::encode_unresolved(
            &input.secret,
            input.installation_id,
            Vec::new(),
        )
        .map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        let account = ProviderAccount::new(
            account_id,
            provider,
            input.name,
            upstream_user_id,
            CODEX_AUTHENTICATION_KIND_OAUTH.to_owned(),
            revision,
            optional_time(input.access_token_expires_at),
        )
        .with_profile(email, upstream_account_id, plan_type)
        .with_account_facts(
            input.enabled,
            if has_upstream_user_id {
                CredentialState::Ready
            } else {
                CredentialState::Unknown
            },
            QuotaState::unknown(),
            (!has_upstream_user_id).then_some(AccountErrorReason::AccountUnverified),
            None,
        )
        .with_refresh_schedule(
            input.secret.refresh_token.is_some(),
            optional_time(input.next_refresh_at),
        );
        Ok(NewProviderAccount {
            account,
            credential,
        })
    }

    /// 严格输出可被 CPR 导入逻辑直接读取的 canonical 文档。
    pub fn format_cpr_export(
        &self,
        items: Vec<ExportManagedCodexCredential>,
    ) -> Result<CodexCprExportDocument, CodexCredentialAdminError> {
        if items.is_empty() || items.len() > MAX_BATCH {
            return Err(CodexCredentialAdminError::InvalidInput);
        }
        let mut ids = BTreeSet::new();
        let mut accounts = Vec::with_capacity(items.len());
        for item in items {
            let account = item.current.account;
            if account.provider().as_str() != PROVIDER_NAME
                || item.added_at > item.updated_at
                || !ids.insert(account.id().clone())
            {
                return Err(CodexCredentialAdminError::InvalidInput);
            }
            let data = CodexCredentialCodec::decode_complete(&item.current.credential)
                .map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
            let common = CodexCprExportCommon {
                id: account.id().as_str().to_owned(),
                email: account.email().map(str::to_owned),
                account_id: account.upstream_account_id().map(str::to_owned),
                user_id: account.upstream_user_id().map(str::to_owned),
                label: Some(account.name().to_owned()),
                plan_type: account.plan_type().map(str::to_owned),
                status: cpr_status(&account),
                added_at: china_rfc3339(item.added_at),
                updated_at: china_rfc3339(item.updated_at),
            };
            let exported = match data {
                CodexCredentialData::OAuth(data) => {
                    if account.authentication_kind() != CODEX_AUTHENTICATION_KIND_OAUTH
                        || account.has_refresh_token() != data.refresh_token.is_some()
                    {
                        return Err(CodexCredentialAdminError::InvalidCredential);
                    }
                    CodexCprOAuthExportAccount {
                        common,
                        access_token: data.access_token,
                        refresh_token: data.refresh_token,
                        id_token: data.id_token,
                        access_token_expires_at: account
                            .access_token_expires_at()
                            .map(DateTime::<Utc>::from)
                            .map(|value| value.to_rfc3339()),
                    }
                }
            };
            accounts.push(exported);
        }
        Ok(CodexCprExportDocument {
            source_format: "cpr",
            accounts,
        })
    }

    pub fn prepare_rotation(
        &self,
        input: RotateManagedCodexCredential,
    ) -> Result<PreparedCodexCredentialRotation, CodexCredentialAdminError> {
        self.prepare_oauth_rotation(input, false)
    }

    /// 构建一次成功 RT exchange 的 CAS 写入，保留已存账号身份资料。
    ///
    /// Refresh endpoint 已经完成本次 token 交换的授权；这里不对新 access
    /// token 重新执行身份验证。
    pub(crate) fn prepare_refreshed_oauth_rotation(
        &self,
        current: LoadedCredential,
        secret: CodexOAuthSecret,
        access_token_expires_at: Option<DateTime<Utc>>,
        next_refresh_at: Option<DateTime<Utc>>,
    ) -> Result<PreparedCodexCredentialRotation, CodexCredentialAdminError> {
        if current.account.provider().as_str() != PROVIDER_NAME
            || current.account.authentication_kind() != CODEX_AUTHENTICATION_KIND_OAUTH
        {
            return Err(CodexCredentialAdminError::InvalidCredential);
        }
        let mut data = CodexCredentialCodec::decode_complete(&current.credential)
            .map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        let oauth = data
            .oauth_mut()
            .ok_or(CodexCredentialAdminError::InvalidCredential)?;
        oauth.access_token = secret.access_token.expose_secret().to_owned();
        oauth.refresh_token = secret
            .refresh_token
            .as_ref()
            .map(|value| value.expose_secret().to_owned());
        oauth.id_token = secret
            .id_token
            .as_ref()
            .map(|value| value.expose_secret().to_owned());
        let credential = CodexCredentialCodec::encode_complete(data)
            .map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        let profile = ProviderAccountUpdate {
            account_id: current.account.id().clone(),
            name: current.account.name().to_owned(),
            email: current.account.email().map(str::to_owned),
            plan_type: current.account.plan_type().map(str::to_owned),
        };
        let credential = CredentialCasUpdate::new(
            current.account.id().clone(),
            current.account.revision(),
            profile.clone(),
            credential,
            secret.refresh_token.is_some(),
            optional_time(access_token_expires_at),
            optional_time(next_refresh_at),
        )
        .map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        Ok(PreparedCodexCredentialRotation {
            profile,
            credential,
            replacement_identity: None,
            refresh_guards: None,
        })
    }

    fn prepare_oauth_rotation(
        &self,
        input: RotateManagedCodexCredential,
        replace_identity: bool,
    ) -> Result<PreparedCodexCredentialRotation, CodexCredentialAdminError> {
        let access_token_expires_at = input.verified_account.access_token_expires_at;
        let mut data = CodexCredentialCodec::decode_complete(&input.current.credential)
            .map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        let oauth = data
            .oauth_mut()
            .ok_or(CodexCredentialAdminError::InvalidCredential)?;
        if input.current.account.provider().as_str() != PROVIDER_NAME
            || input.current.account.authentication_kind() != CODEX_AUTHENTICATION_KIND_OAUTH
        {
            return Err(CodexCredentialAdminError::InvalidCredential);
        }
        let replacement_identity = replace_identity.then(|| {
            ProviderAccountIdentity::new(
                input.verified_account.chatgpt_user_id.clone(),
                Some(input.verified_account.chatgpt_account_id.clone()),
            )
        });
        if replace_identity {
            oauth.principal = Some(CodexCredentialPrincipal {
                oauth_subject: input.verified_account.oauth_subject.clone(),
                poid: input.verified_account.poid.clone(),
            });
        }
        oauth.access_token = input.secret.access_token.expose_secret().to_owned();
        oauth.refresh_token = input
            .secret
            .refresh_token
            .as_ref()
            .map(|value| value.expose_secret().to_owned());
        oauth.id_token = input
            .secret
            .id_token
            .as_ref()
            .map(|value| value.expose_secret().to_owned());
        let credential = CodexCredentialCodec::encode_complete(data)
            .map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        let profile = ProviderAccountUpdate {
            account_id: input.current.account.id().clone(),
            name: input.current.account.name().to_owned(),
            email: input.verified_account.email,
            plan_type: input.verified_account.plan_type,
        };
        let credential = CredentialCasUpdate::new(
            input.current.account.id().clone(),
            input.current.account.revision(),
            profile.clone(),
            credential,
            input.secret.refresh_token.is_some(),
            optional_time(access_token_expires_at),
            optional_time(input.next_refresh_at),
        )
        .map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        Ok(PreparedCodexCredentialRotation {
            profile,
            credential,
            replacement_identity,
            refresh_guards: None,
        })
    }
}

/// 有状态的 Codex 手工刷新边界；消费调用方刚读取的当前 credential 并准备 CAS。
pub struct CodexCredentialAdminService {
    refresher: Arc<dyn TokenRefresher>,
    leases: Arc<dyn ProviderLeasePort>,
    runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
}

impl fmt::Debug for CodexCredentialAdminService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredentialAdminService")
            .field("refresher", &"TokenRefresher")
            .field("leases", &"ProviderLeasePort")
            .field("runtime_policy", &"ProviderRuntimePolicyPort")
            .finish()
    }
}

impl CodexCredentialAdminService {
    pub fn new(
        refresher: Arc<dyn TokenRefresher>,
        leases: Arc<dyn ProviderLeasePort>,
        runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
    ) -> Self {
        Self {
            refresher,
            leases,
            runtime_policy,
        }
    }

    /// 官方 RT exchange；结果由 App 在同一 revision/audit 事务中提交。
    pub async fn manual_refresh(
        &self,
        current: LoadedCredential,
    ) -> Result<PreparedCodexCredentialRotation, CodexCredentialAdminError> {
        let account_id = current.account.id().clone();
        let expected_revision = current.account.revision();
        if current.account.provider().as_str() != PROVIDER_NAME {
            return Err(CodexCredentialAdminError::NotFound);
        }
        let runtime = CodexCredentialCodec::decode(&current.credential)
            .map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        let oauth = runtime
            .authentication
            .oauth()
            .ok_or(CodexCredentialAdminError::MissingRefreshToken)?;
        let refresh_token = oauth
            .refresh_token
            .as_ref()
            .ok_or(CodexCredentialAdminError::MissingRefreshToken)?;
        let policy = self
            .runtime_policy
            .load_refresh_policy()
            .await
            .map_err(|_| CodexCredentialAdminError::RefreshUnavailable)?;
        let capacity_guard = match self
            .leases
            .try_acquire(ProviderLeaseRequest::RefreshCapacity(
                ProviderRefreshCapacityRequest::new(policy.concurrency()),
            ))
            .await
            .map_err(|_| CodexCredentialAdminError::RefreshUnavailable)?
        {
            ProviderLeaseAcquisition::Acquired(guard) => guard,
            ProviderLeaseAcquisition::Busy { .. } => {
                return Err(CodexCredentialAdminError::RefreshLeaseUnavailable);
            }
        };
        let account_guard = match self
            .leases
            .try_acquire(ProviderLeaseRequest::Refresh(
                ProviderRefreshLeaseRequest::new(account_id.clone(), expected_revision),
            ))
            .await
            .map_err(|_| CodexCredentialAdminError::RefreshUnavailable)?
        {
            ProviderLeaseAcquisition::Acquired(guard) => guard,
            ProviderLeaseAcquisition::Busy { .. } => {
                return Err(CodexCredentialAdminError::RefreshLeaseUnavailable);
            }
        };
        let tokens = self
            .refresher
            .refresh(refresh_token.expose_secret())
            .await
            .inspect_err(|error| log_manual_refresh_failure(&account_id, error))
            .map_err(map_refresh_failure)?;
        let access_token_expires_at = match tokens.access_token.as_deref() {
            Some(access_token) => parse_access_token_expiration(access_token),
            None => current
                .account
                .access_token_expires_at()
                .map(DateTime::<Utc>::from),
        };
        let secret = CodexOAuthSecret {
            access_token: tokens
                .access_token
                .map(SecretString::from)
                .unwrap_or_else(|| oauth.access_token.clone()),
            refresh_token: tokens
                .refresh_token
                .map(SecretString::from)
                .or_else(|| oauth.refresh_token.clone()),
            id_token: tokens
                .id_token
                .map(SecretString::from)
                .or_else(|| oauth.id_token.clone()),
        };
        Self::record_recovery_log(
            CodexOAuthRecoveryOperation::ManualRefresh,
            Some(account_id.as_str()),
            &secret,
        );
        // 正常预刷新由 worker 读取当时的 runtime policy 动态判断；这里仅清除
        // 之前瞬态失败留下的 retry-not-before。
        let next_refresh_at = None;
        let mut prepared = CodexCredentialAdmin.prepare_refreshed_oauth_rotation(
            current,
            secret,
            access_token_expires_at,
            next_refresh_at,
        )?;
        prepared.refresh_guards = Some(ProviderRefreshGuards {
            _capacity: capacity_guard,
            _account: account_guard,
        });
        Ok(prepared)
    }

    /// 归一导入 OAuth 凭据到唯一 `NewProviderAccount` 写入路径。
    ///
    /// OAuth 导入先取得 access token（直接提供或 RT exchange），再按官方
    /// `parse_chatgpt_jwt_claims` 从 ID token/access token 本地投影账号资料。
    pub async fn prepare_import_document(
        &self,
        payload: Value,
    ) -> Result<PreparedCodexAccountImport, CodexCredentialAdminError> {
        if serde_json::to_vec(&payload)
            .map_err(|_| CodexCredentialAdminError::InvalidInput)?
            .len()
            > MAX_IMPORT_DOCUMENT_BYTES
        {
            return Err(CodexCredentialAdminError::InvalidInput);
        }
        let candidates = parse_import_document(&payload)?;
        if candidates.is_empty() || candidates.len() > MAX_BATCH {
            return Err(CodexCredentialAdminError::InvalidInput);
        }
        let mut accounts = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let account_id = format!("acct_{}", uuid::Uuid::now_v7().simple());
            let typed_account_id = ProviderAccountId::new(account_id.clone())
                .map_err(|_| CodexCredentialAdminError::InvalidInput)?;
            let (secret, access_token_expires_at) = self
                .resolve_import_tokens(
                    &typed_account_id,
                    candidate.authentication.access_token.clone(),
                    candidate.authentication.refresh_token.clone(),
                    candidate.authentication.id_token.clone(),
                )
                .await?;
            // 与官方 `parse_chatgpt_jwt_claims` 一致：先读 ID token，缺失字段
            // 再由 access token 补齐。解析失败不阻塞 token 入库。
            let id_metadata = secret
                .id_token
                .as_ref()
                .and_then(|token| parse_chatgpt_jwt_claims(token.expose_secret()).ok())
                .unwrap_or_default();
            let access_metadata =
                parse_chatgpt_jwt_claims(secret.access_token.expose_secret()).unwrap_or_default();
            let metadata = CodexOAuthMetadata {
                email: id_metadata.email.or(access_metadata.email),
                chatgpt_plan_type: id_metadata
                    .chatgpt_plan_type
                    .or(access_metadata.chatgpt_plan_type),
                chatgpt_user_id: id_metadata
                    .chatgpt_user_id
                    .or(access_metadata.chatgpt_user_id),
                chatgpt_account_id: id_metadata
                    .chatgpt_account_id
                    .or(access_metadata.chatgpt_account_id),
            };
            let prepared =
                CodexCredentialAdmin.prepare_unresolved_oauth(UnresolvedCodexOAuthCredential {
                    account_id,
                    name: candidate
                        .name
                        .clone()
                        .or_else(|| candidate.email.clone())
                        .or_else(|| metadata.email.clone())
                        .filter(|name| !name.trim().is_empty())
                        .unwrap_or_else(|| "Codex OAuth".to_owned()),
                    installation_id: uuid::Uuid::new_v4().to_string(),
                    secret,
                    metadata,
                    access_token_expires_at,
                    next_refresh_at: None,
                    enabled: true,
                })?;
            accounts.push(prepared);
        }
        Ok(PreparedCodexAccountImport { accounts })
    }

    async fn resolve_import_tokens(
        &self,
        account_id: &ProviderAccountId,
        access_token: Option<String>,
        refresh_token: Option<String>,
        id_token: Option<String>,
    ) -> Result<(CodexOAuthSecret, Option<DateTime<Utc>>), CodexCredentialAdminError> {
        let id_token = id_token.map(SecretString::from);
        if let Some(access_token) = access_token {
            let access_token_expires_at = parse_access_token_expiration(&access_token);
            let secret = CodexOAuthSecret {
                access_token: SecretString::from(access_token),
                refresh_token: refresh_token.map(SecretString::from),
                id_token,
            };
            Self::record_recovery_log(
                CodexOAuthRecoveryOperation::ImportDirect,
                Some(account_id.as_str()),
                &secret,
            );
            return Ok((secret, access_token_expires_at));
        }
        let refresh_token = refresh_token.ok_or(CodexCredentialAdminError::InvalidCredential)?;
        let tokens = self
            .refresher
            .refresh(&refresh_token)
            .await
            .map_err(map_refresh_failure)?;
        let access_token = tokens
            .access_token
            .ok_or(CodexCredentialAdminError::InvalidCredential)?;
        let access_token_expires_at = parse_access_token_expiration(&access_token);
        let secret = CodexOAuthSecret {
            access_token: SecretString::from(access_token),
            // RT 未轮换时仍保留导入提供的 RT，保证本次补全不会丢失后续刷新能力。
            refresh_token: tokens
                .refresh_token
                .map(SecretString::from)
                .or_else(|| Some(SecretString::from(refresh_token))),
            id_token: tokens.id_token.map(SecretString::from).or(id_token),
        };
        Self::record_recovery_log(
            CodexOAuthRecoveryOperation::ImportRefreshToken,
            Some(account_id.as_str()),
            &secret,
        );
        Ok((secret, access_token_expires_at))
    }

    fn record_recovery_log(
        operation: CodexOAuthRecoveryOperation,
        account_id: Option<&str>,
        secret: &CodexOAuthSecret,
    ) {
        record_oauth_recovery(
            operation,
            account_id,
            secret.access_token.expose_secret(),
            secret
                .refresh_token
                .as_ref()
                .map(ExposeSecret::expose_secret),
        );
    }
}

fn optional_time(value: Option<chrono::DateTime<chrono::Utc>>) -> Option<SystemTime> {
    value.map(SystemTime::from)
}

fn cpr_status(account: &ProviderAccount) -> &'static str {
    if !account.enabled() {
        return "disabled";
    }
    if account.quota().is_exhausted() {
        return "quota_exhausted";
    }
    match account.credential_state() {
        CredentialState::Expired | CredentialState::Invalid => "expired",
        CredentialState::Banned => "banned",
        CredentialState::Unknown | CredentialState::Ready => "active",
    }
}

fn china_rfc3339(value: DateTime<Utc>) -> String {
    value
        .with_timezone(&FixedOffset::east_opt(8 * 60 * 60).expect("valid China offset"))
        .to_rfc3339()
}

fn map_refresh_failure(error: RefreshFailure) -> CodexCredentialAdminError {
    match error {
        RefreshFailure::InvalidGrant { message, .. } => {
            CodexCredentialAdminError::RefreshRejected { message }
        }
        RefreshFailure::Banned { message, .. } => {
            CodexCredentialAdminError::AccountBanned { message }
        }
        RefreshFailure::RetryableTransport { .. } => CodexCredentialAdminError::RefreshUnavailable,
        RefreshFailure::Transport { message, .. } => {
            CodexCredentialAdminError::RefreshAmbiguous { message }
        }
    }
}

fn log_manual_refresh_failure(account_id: &ProviderAccountId, error: &RefreshFailure) {
    let upstream = error.upstream();
    tracing::warn!(
        account_id = %account_id,
        failure_class = error.classification(),
        upstream_message = ?error.message(),
        upstream_status = ?upstream.map(super::token_client::RefreshUpstreamFailure::status),
        upstream_code = ?upstream.and_then(super::token_client::RefreshUpstreamFailure::code),
        upstream_type = ?upstream.and_then(super::token_client::RefreshUpstreamFailure::error_type),
        upstream_body = ?upstream.map(super::token_client::RefreshUpstreamFailure::body),
        "OpenAI OAuth manual refresh failed"
    );
}

fn parse_import_document(
    payload: &Value,
) -> Result<Vec<ParsedCodexImportAccount>, CodexCredentialAdminError> {
    let mut accounts = Vec::new();
    for value in import_account_values(payload)? {
        if !is_openai_oauth_candidate(value) {
            continue;
        }
        accounts.push(parse_oauth_import_account(value)?);
    }
    Ok(accounts)
}

fn parse_oauth_import_account(
    value: &Value,
) -> Result<ParsedCodexImportAccount, CodexCredentialAdminError> {
    let credentials = value.get("credentials").unwrap_or(value);
    Ok(ParsedCodexImportAccount {
        name: first_string(value, &["name", "label"]),
        email: first_string(value, &["email"]).or_else(|| first_string(credentials, &["email"])),
        authentication: parse_oauth_import_tokens(value)?,
    })
}

fn import_account_values(payload: &Value) -> Result<Vec<&Value>, CodexCredentialAdminError> {
    if let Some(accounts) = payload.get("accounts") {
        return accounts
            .as_array()
            .map(|accounts| accounts.iter().collect())
            .ok_or(CodexCredentialAdminError::InvalidInput);
    }
    if let Some(accounts) = payload.as_array() {
        return Ok(accounts.iter().collect());
    }
    Ok(vec![payload])
}

fn parse_oauth_import_tokens(
    value: &Value,
) -> Result<ParsedCodexAuthentication, CodexCredentialAdminError> {
    let mut access_token = None;
    let mut refresh_token = None;
    let mut id_token = None;
    let mut pending = vec![value];
    while let Some(current) = pending.pop() {
        match current {
            Value::Object(object) => {
                for (key, value) in object {
                    let token = match key.as_str() {
                        "accessToken" | "access_token" => &mut access_token,
                        "refreshToken" | "refresh_token" => &mut refresh_token,
                        "idToken" | "id_token" => &mut id_token,
                        _ => {
                            pending.push(value);
                            continue;
                        }
                    };
                    if let Some(value) = value.as_str() {
                        *token = Some(value.to_owned());
                    }
                }
            }
            Value::Array(values) => pending.extend(values),
            _ => {}
        }
    }
    if access_token.is_none() && refresh_token.is_none() {
        return Err(CodexCredentialAdminError::InvalidCredential);
    }
    Ok(ParsedCodexAuthentication {
        access_token,
        refresh_token,
        id_token,
    })
}

fn is_openai_oauth_candidate(value: &Value) -> bool {
    let Some(account) = value.as_object() else {
        return false;
    };
    if let Some(provider) = account
        .get("platform")
        .or_else(|| account.get("provider"))
        .and_then(Value::as_str)
    {
        return provider.eq_ignore_ascii_case("openai") || provider.eq_ignore_ascii_case("codex");
    }
    account
        .get("type")
        .and_then(Value::as_str)
        .is_none_or(|kind| {
            kind.eq_ignore_ascii_case("openai")
                || kind.eq_ignore_ascii_case("codex")
                || kind.eq_ignore_ascii_case("oauth")
        })
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
