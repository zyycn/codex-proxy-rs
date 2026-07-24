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
    AccountAvailability, CredentialCasUpdate, CredentialRevision, LoadedCredential,
    NewProviderAccount, ProviderAccount, ProviderAccountId, ProviderAccountUpdate,
};
use gateway_core::error::StoreErrorKind;
use gateway_core::provider_ports::{
    ProviderLeaseAcquisition, ProviderLeaseGuard, ProviderLeasePort, ProviderLeaseRequest,
    ProviderRefreshCapacityRequest, ProviderRefreshLeaseRequest, ProviderRefreshPolicy,
    ProviderRuntimePolicyPort,
};
use gateway_core::routing::ProviderKind;
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use super::identity::{
    CodexAccountIdentityVerifier, CodexIdentityExpectation, CodexIdentityVerificationError,
};
use super::repository::CodexCredentialRepository;
use super::security::CodexCredentialCodec;
use super::token_client::{RefreshFailure, TokenRefresher};
use super::types::{
    CODEX_AUTHENTICATION_KIND_AGENT_IDENTITY, CODEX_AUTHENTICATION_KIND_OAUTH, CodexAccountProfile,
    CodexAgentIdentityAuthMode, CodexAgentIdentityCredentialData, CodexCredentialData,
    CodexOAuthSecret,
};

const PROVIDER_NAME: &str = "openai";
const CODEX_CREDENTIAL_SCHEMA_VERSION: u32 = 1;
const MAX_BATCH: usize = 200;
const MAX_IMPORT_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

const CPR_CONTAINER_KEYS: &[&str] = &["sourceFormat", "source_format", "accounts"];
const CPR_ACCOUNT_KEYS: &[&str] = &[
    "id",
    "email",
    "accountId",
    "account_id",
    "userId",
    "user_id",
    "label",
    "planType",
    "plan_type",
    "token",
    "at",
    "accessToken",
    "access_token",
    "refreshToken",
    "refresh_token",
    "accessTokenExpiresAt",
    "access_token_expires_at",
    "status",
    "addedAt",
    "added_at",
    "updatedAt",
    "updated_at",
];

pub struct ImportCodexOAuthCredential {
    pub account_id: String,
    pub name: String,
    pub secret: CodexOAuthSecret,
    pub verified_account: CodexAccountProfile,
    pub next_refresh_at: Option<DateTime<Utc>>,
    pub enabled: bool,
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
    id: Option<String>,
    name: Option<String>,
    email: Option<String>,
    plan_type: Option<String>,
    chatgpt_account_id: Option<String>,
    chatgpt_user_id: Option<String>,
    authentication: ParsedCodexAuthentication,
    status: Option<String>,
}

enum ParsedCodexAuthentication {
    OAuth {
        access_token: Option<String>,
        refresh_token: Option<String>,
        id_token: Option<String>,
    },
    AgentIdentity {
        runtime_id: String,
        private_key: String,
        task_id: Option<String>,
    },
}

impl fmt::Debug for ParsedCodexAuthentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OAuth {
                access_token,
                refresh_token,
                id_token,
            } => formatter
                .debug_struct("OAuth")
                .field("access_token", &access_token.as_ref().map(|_| "<redacted>"))
                .field(
                    "refresh_token",
                    &refresh_token.as_ref().map(|_| "<redacted>"),
                )
                .field("id_token", &id_token.as_ref().map(|_| "<redacted>"))
                .finish(),
            Self::AgentIdentity {
                runtime_id: _,
                private_key: _,
                task_id,
            } => formatter
                .debug_struct("AgentIdentity")
                .field("runtime_id", &"<redacted>")
                .field("private_key", &"<redacted>")
                .field("task_id", &task_id.as_ref().map(|_| "<redacted>"))
                .finish(),
        }
    }
}

impl fmt::Debug for ParsedCodexImportAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParsedCodexImportAccount")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("email", &self.email.as_ref().map(|_| "<redacted>"))
            .field("plan_type", &self.plan_type)
            .field(
                "chatgpt_account_id",
                &self.chatgpt_account_id.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "chatgpt_user_id",
                &self.chatgpt_user_id.as_ref().map(|_| "<redacted>"),
            )
            .field("authentication", &self.authentication)
            .field("status", &self.status)
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

/// CPR canonical 账号导出文档；只允许显式序列化，Debug 永不输出 token。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexCprExportDocument {
    source_format: &'static str,
    accounts: Vec<CodexCprExportAccount>,
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
struct CodexCprExportAccount {
    id: String,
    email: Option<String>,
    account_id: Option<String>,
    user_id: Option<String>,
    label: Option<String>,
    plan_type: Option<String>,
    token: String,
    refresh_token: Option<String>,
    access_token_expires_at: Option<String>,
    status: &'static str,
    added_at: String,
    updated_at: String,
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
        PreparedCodexCredentialRotationGuard,
    ) {
        (
            self.profile,
            self.credential,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CodexCredentialAdminError {
    #[error("Codex account input is invalid")]
    InvalidInput,
    #[error("Codex account identity does not match the existing account")]
    IdentityMismatch,
    #[error("Codex credential JSON is invalid")]
    InvalidCredential,
    #[error("Codex account was not found")]
    NotFound,
    #[error("Codex credential revision is stale")]
    RevisionConflict,
    #[error("Codex account store is unavailable")]
    StoreUnavailable,
    #[error("Codex account has no refresh token")]
    MissingRefreshToken,
    #[error("Codex refresh lease is unavailable")]
    RefreshLeaseUnavailable,
    #[error("Codex refresh token was rejected")]
    RefreshRejected,
    #[error("Codex account is banned")]
    AccountBanned,
    #[error("Codex refresh service is unavailable")]
    RefreshUnavailable,
    #[error("Codex refresh send state is ambiguous")]
    RefreshAmbiguous,
    #[error("Codex refreshed identity was rejected")]
    IdentityRejected,
    #[error("Codex identity verification is unavailable")]
    IdentityUnavailable,
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
        let access_token_expires_at =
            required_time(input.verified_account.access_token_expires_at)?;
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
            upstream_user_id,
            CODEX_AUTHENTICATION_KIND_OAUTH.to_owned(),
            revision,
            Some(access_token_expires_at),
        )
        .with_profile(
            input.verified_account.email,
            Some(input.verified_account.chatgpt_account_id),
            input.verified_account.plan_type,
        )
        .with_runtime_state(input.enabled, AccountAvailability::Ready, None)
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
            let CodexCredentialData::OAuth(data) = data else {
                return Err(CodexCredentialAdminError::InvalidCredential);
            };
            if account.authentication_kind() != CODEX_AUTHENTICATION_KIND_OAUTH
                || account.has_refresh_token() != data.refresh_token.is_some()
            {
                return Err(CodexCredentialAdminError::InvalidCredential);
            }
            accounts.push(CodexCprExportAccount {
                id: account.id().as_str().to_owned(),
                email: account.email().map(str::to_owned),
                account_id: account.upstream_account_id().map(str::to_owned),
                user_id: Some(account.upstream_user_id().to_owned()),
                label: Some(account.name().to_owned()),
                plan_type: account.plan_type().map(str::to_owned),
                token: data.access_token,
                refresh_token: data.refresh_token,
                access_token_expires_at: account
                    .access_token_expires_at()
                    .map(DateTime::<Utc>::from)
                    .map(|value| value.to_rfc3339()),
                status: cpr_status(&account),
                added_at: china_rfc3339(item.added_at),
                updated_at: china_rfc3339(item.updated_at),
            });
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
        let access_token_expires_at =
            required_time(input.verified_account.access_token_expires_at)?;
        let mut data = CodexCredentialCodec::decode_complete(&input.current.credential)
            .map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        let oauth = data
            .oauth_mut()
            .ok_or(CodexCredentialAdminError::IdentityMismatch)?;
        if input.current.account.provider().as_str() != PROVIDER_NAME
            || input.current.account.authentication_kind() != CODEX_AUTHENTICATION_KIND_OAUTH
            || input.current.account.upstream_account_id()
                != Some(input.verified_account.chatgpt_account_id.as_str())
            || input.current.account.upstream_user_id() != input.verified_account.chatgpt_user_id
            || oauth.principal.oauth_subject != input.verified_account.oauth_subject
            || oauth.principal.poid != input.verified_account.poid
        {
            return Err(CodexCredentialAdminError::IdentityMismatch);
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
            Some(access_token_expires_at),
            optional_time(input.next_refresh_at),
        )
        .map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        Ok(PreparedCodexCredentialRotation {
            profile,
            credential,
            refresh_guards: None,
        })
    }
}

/// 有状态的 Codex 手工刷新边界；只读取 Store 并准备 CAS，不自行持久化。
pub struct CodexCredentialAdminService {
    repository: CodexCredentialRepository,
    refresher: Arc<dyn TokenRefresher>,
    verifier: Arc<dyn CodexAccountIdentityVerifier>,
    leases: Arc<dyn ProviderLeasePort>,
    runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
}

impl fmt::Debug for CodexCredentialAdminService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredentialAdminService")
            .field("repository", &"CodexCredentialRepository")
            .field("refresher", &"TokenRefresher")
            .field("verifier", &"CodexAccountIdentityVerifier")
            .field("leases", &"ProviderLeasePort")
            .field("runtime_policy", &"ProviderRuntimePolicyPort")
            .finish()
    }
}

impl CodexCredentialAdminService {
    pub fn new(
        repository: CodexCredentialRepository,
        refresher: Arc<dyn TokenRefresher>,
        verifier: Arc<dyn CodexAccountIdentityVerifier>,
        leases: Arc<dyn ProviderLeasePort>,
        runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
    ) -> Self {
        Self {
            repository,
            refresher,
            verifier,
            leases,
            runtime_policy,
        }
    }

    /// 官方 RT exchange + AT 身份验证；结果由 App 在同一 revision/audit 事务中提交。
    pub async fn manual_refresh(
        &self,
        account_id: ProviderAccountId,
        expected_revision: CredentialRevision,
    ) -> Result<PreparedCodexCredentialRotation, CodexCredentialAdminError> {
        let current = self
            .repository
            .store()
            .load_credential(&account_id, expected_revision)
            .await
            .map_err(map_admin_store_error)?;
        if current.account.provider().as_str() != PROVIDER_NAME
            || current.account.id() != &account_id
            || current.account.revision() != expected_revision
        {
            return Err(CodexCredentialAdminError::NotFound);
        }
        let runtime = CodexCredentialCodec::decode(&current.credential)
            .map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        let expectation = current_identity_expectation(&current.account, &runtime)?;
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
            .map_err(map_refresh_failure)?;
        if tokens.access_token.trim().is_empty() || tokens.expires_in.is_zero() {
            return Err(CodexCredentialAdminError::InvalidCredential);
        }
        let secret = CodexOAuthSecret {
            access_token: SecretString::from(tokens.access_token),
            refresh_token: tokens
                .refresh_token
                .map(SecretString::from)
                .or_else(|| oauth.refresh_token.clone()),
            id_token: oauth.id_token.clone(),
        };
        let verified_account = self
            .verifier
            .verify(&secret, &expectation)
            .await
            .and_then(super::identity::CodexIdentityVerification::into_complete)
            .map_err(map_identity_error)?;
        let next_refresh_at = refresh_time(
            policy,
            &account_id,
            verified_account.access_token_expires_at,
            secret.refresh_token.is_some(),
        )?;
        let mut prepared = CodexCredentialAdmin.prepare_rotation(RotateManagedCodexCredential {
            current,
            secret,
            verified_account,
            next_refresh_at,
        })?;
        prepared.refresh_guards = Some(ProviderRefreshGuards {
            _capacity: capacity_guard,
            _account: account_guard,
        });
        Ok(prepared)
    }

    /// 按文档结构严格判别账号布局，并归一到唯一 `NewProviderAccount` 写入路径。
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
        let mut account_ids = BTreeSet::new();
        let mut upstream_identities = BTreeSet::new();
        let mut accounts = Vec::with_capacity(candidates.len());
        let mut policy = None;
        for candidate in candidates {
            let account_id = candidate
                .id
                .clone()
                .filter(|id| ProviderAccountId::new(id.clone()).is_ok())
                .unwrap_or_else(|| format!("acct_{}", uuid::Uuid::now_v7().simple()));
            if !account_ids.insert(account_id.clone()) {
                return Err(CodexCredentialAdminError::InvalidInput);
            }
            let (enabled, availability) = import_runtime_state(candidate.status.as_deref())?;
            let mut prepared = match &candidate.authentication {
                ParsedCodexAuthentication::OAuth { .. } => {
                    let (secret, verified_account) =
                        self.verify_import_candidate(&candidate).await?;
                    if !upstream_identities.insert((
                        verified_account.chatgpt_user_id.clone(),
                        verified_account.chatgpt_account_id.clone(),
                    )) {
                        return Err(CodexCredentialAdminError::InvalidInput);
                    }
                    let refresh_policy = if let Some(policy) = policy {
                        policy
                    } else {
                        let loaded = self
                            .runtime_policy
                            .load_refresh_policy()
                            .await
                            .map_err(|_| CodexCredentialAdminError::RefreshUnavailable)?;
                        policy = Some(loaded);
                        loaded
                    };
                    let next_refresh_at = refresh_time(
                        refresh_policy,
                        &ProviderAccountId::new(account_id.clone())
                            .map_err(|_| CodexCredentialAdminError::InvalidInput)?,
                        verified_account.access_token_expires_at,
                        secret.refresh_token.is_some(),
                    )?;
                    CodexCredentialAdmin.prepare_import(ImportCodexOAuthCredential {
                        account_id,
                        name: candidate
                            .name
                            .clone()
                            .filter(|name| !name.trim().is_empty())
                            .unwrap_or_else(|| "Codex OAuth".to_owned()),
                        secret,
                        verified_account,
                        next_refresh_at,
                        enabled,
                    })?
                }
                ParsedCodexAuthentication::AgentIdentity { .. } => {
                    let account =
                        self.prepare_agent_identity_import(&candidate, account_id, enabled)?;
                    if !upstream_identities.insert((
                        account.account.upstream_user_id().to_owned(),
                        account
                            .account
                            .upstream_account_id()
                            .ok_or(CodexCredentialAdminError::InvalidInput)?
                            .to_owned(),
                    )) {
                        return Err(CodexCredentialAdminError::InvalidInput);
                    }
                    account
                }
            };
            prepared.account = prepared
                .account
                .with_runtime_state(enabled, availability, None);
            accounts.push(prepared);
        }
        Ok(PreparedCodexAccountImport { accounts })
    }

    async fn verify_import_candidate(
        &self,
        candidate: &ParsedCodexImportAccount,
    ) -> Result<(CodexOAuthSecret, CodexAccountProfile), CodexCredentialAdminError> {
        let ParsedCodexAuthentication::OAuth {
            access_token,
            refresh_token,
            id_token,
        } = &candidate.authentication
        else {
            return Err(CodexCredentialAdminError::InvalidCredential);
        };
        let access_token = access_token
            .as_deref()
            .map(normalize_bearer)
            .filter(|token| !token.is_empty());
        let refresh_token = refresh_token
            .as_deref()
            .map(normalize_bearer)
            .filter(|token| !token.is_empty());
        let id_token = id_token
            .as_deref()
            .map(normalize_bearer)
            .filter(|token| !token.is_empty());
        let expectation = CodexIdentityExpectation::imported(
            candidate.chatgpt_account_id.clone(),
            candidate.chatgpt_user_id.clone(),
        )
        .map_err(|_| CodexCredentialAdminError::InvalidInput)?;
        if let Some(access_token) = access_token {
            let secret = CodexOAuthSecret {
                access_token: SecretString::from(access_token),
                refresh_token: refresh_token.clone().map(SecretString::from),
                id_token: id_token.clone().map(SecretString::from),
            };
            match self.verifier.verify(&secret, &expectation).await {
                Ok(verification) => {
                    let profile = verification.into_complete().map_err(map_identity_error)?;
                    return Ok((secret, profile));
                }
                Err(CodexIdentityVerificationError::Unavailable) => {
                    return Err(CodexCredentialAdminError::IdentityUnavailable);
                }
                Err(CodexIdentityVerificationError::Rejected) if refresh_token.is_none() => {
                    return Err(CodexCredentialAdminError::IdentityRejected);
                }
                Err(CodexIdentityVerificationError::Rejected) => {}
            }
        }
        let refresh_token = refresh_token.ok_or(CodexCredentialAdminError::InvalidCredential)?;
        let tokens = self
            .refresher
            .refresh(&refresh_token)
            .await
            .map_err(map_refresh_failure)?;
        if tokens.access_token.trim().is_empty() || tokens.expires_in.is_zero() {
            return Err(CodexCredentialAdminError::InvalidCredential);
        }
        // 导入时已经消费来源 RT；只有上游明确轮换返回的新 RT 才能继续保存。
        let secret = CodexOAuthSecret {
            access_token: SecretString::from(tokens.access_token),
            refresh_token: tokens.refresh_token.map(SecretString::from),
            id_token: id_token.map(SecretString::from),
        };
        let profile = self
            .verifier
            .verify(&secret, &expectation)
            .await
            .and_then(super::identity::CodexIdentityVerification::into_complete)
            .map_err(map_identity_error)?;
        Ok((secret, profile))
    }

    fn prepare_agent_identity_import(
        &self,
        candidate: &ParsedCodexImportAccount,
        account_id: String,
        enabled: bool,
    ) -> Result<NewProviderAccount, CodexCredentialAdminError> {
        let ParsedCodexAuthentication::AgentIdentity {
            runtime_id,
            private_key,
            task_id,
        } = &candidate.authentication
        else {
            return Err(CodexCredentialAdminError::InvalidCredential);
        };
        let upstream_account_id = candidate
            .chatgpt_account_id
            .clone()
            .ok_or(CodexCredentialAdminError::InvalidInput)?;
        let upstream_user_id = candidate
            .chatgpt_user_id
            .clone()
            .ok_or(CodexCredentialAdminError::InvalidInput)?;
        let account_id = ProviderAccountId::new(account_id)
            .map_err(|_| CodexCredentialAdminError::InvalidInput)?;
        let provider = ProviderKind::new(PROVIDER_NAME)
            .map_err(|_| CodexCredentialAdminError::InvalidInput)?;
        let revision =
            CredentialRevision::new(1).map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        let credential =
            CodexCredentialCodec::encode_agent_identity(CodexAgentIdentityCredentialData {
                schema_version: CODEX_CREDENTIAL_SCHEMA_VERSION,
                auth_mode: CodexAgentIdentityAuthMode::AgentIdentity,
                installation_id: uuid::Uuid::new_v4().to_string(),
                agent_runtime_id: runtime_id.clone(),
                agent_private_key: private_key.clone(),
                task_id: task_id.clone(),
                cookies: Vec::new(),
            })
            .map_err(|_| CodexCredentialAdminError::InvalidCredential)?;
        let name = candidate
            .name
            .clone()
            .or_else(|| candidate.email.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "OpenAI Agent Identity".to_owned());
        let account = ProviderAccount::new(
            account_id,
            provider,
            name,
            upstream_user_id,
            CODEX_AUTHENTICATION_KIND_AGENT_IDENTITY.to_owned(),
            revision,
            None,
        )
        .with_profile(
            candidate.email.clone(),
            Some(upstream_account_id),
            candidate.plan_type.clone(),
        )
        .with_runtime_state(enabled, AccountAvailability::Ready, None)
        .with_refresh_schedule(false, None);
        Ok(NewProviderAccount {
            account,
            credential,
        })
    }
}

fn current_identity_expectation(
    account: &ProviderAccount,
    credential: &super::security::CodexRuntimeCredential,
) -> Result<CodexIdentityExpectation, CodexCredentialAdminError> {
    let account_id = account
        .upstream_account_id()
        .ok_or(CodexCredentialAdminError::InvalidCredential)?;
    let principal = credential
        .principal
        .as_ref()
        .ok_or(CodexCredentialAdminError::InvalidCredential)?;
    CodexIdentityExpectation::current(
        principal.oauth_subject.clone(),
        principal.poid.clone(),
        account_id.to_owned(),
        account.upstream_user_id().to_owned(),
        credential.installation_id.clone(),
    )
    .map_err(|_| CodexCredentialAdminError::InvalidCredential)
}

fn required_time(
    value: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<SystemTime, CodexCredentialAdminError> {
    value
        .map(SystemTime::from)
        .ok_or(CodexCredentialAdminError::InvalidCredential)
}

fn optional_time(value: Option<chrono::DateTime<chrono::Utc>>) -> Option<SystemTime> {
    value.map(SystemTime::from)
}

pub(crate) fn refresh_time(
    policy: ProviderRefreshPolicy,
    account_id: &ProviderAccountId,
    access_token_expires_at: Option<DateTime<Utc>>,
    has_refresh_token: bool,
) -> Result<Option<DateTime<Utc>>, CodexCredentialAdminError> {
    if !has_refresh_token {
        return Ok(None);
    }
    let expires_at = required_time(access_token_expires_at)?;
    policy
        .next_attempt_at(account_id, expires_at, SystemTime::now())
        .map(DateTime::<Utc>::from)
        .map(Some)
        .map_err(|_| CodexCredentialAdminError::InvalidCredential)
}

fn cpr_status(account: &ProviderAccount) -> &'static str {
    if !account.enabled() {
        return "disabled";
    }
    match account.availability() {
        AccountAvailability::QuotaExhausted => "quota_exhausted",
        AccountAvailability::Expired | AccountAvailability::Invalid => "expired",
        AccountAvailability::Banned => "banned",
        AccountAvailability::Unknown
        | AccountAvailability::Ready
        | AccountAvailability::Cooldown => "active",
    }
}

fn china_rfc3339(value: DateTime<Utc>) -> String {
    value
        .with_timezone(&FixedOffset::east_opt(8 * 60 * 60).expect("valid China offset"))
        .to_rfc3339()
}

fn map_admin_store_error(error: gateway_core::error::StoreError) -> CodexCredentialAdminError {
    match error.kind() {
        StoreErrorKind::Conflict => CodexCredentialAdminError::RevisionConflict,
        StoreErrorKind::Unavailable => CodexCredentialAdminError::StoreUnavailable,
        StoreErrorKind::InvalidState | StoreErrorKind::InvalidData => {
            CodexCredentialAdminError::NotFound
        }
        _ => CodexCredentialAdminError::StoreUnavailable,
    }
}

const fn map_refresh_failure(error: RefreshFailure) -> CodexCredentialAdminError {
    match error {
        RefreshFailure::InvalidGrant => CodexCredentialAdminError::RefreshRejected,
        RefreshFailure::Banned => CodexCredentialAdminError::AccountBanned,
        RefreshFailure::RetryableTransport => CodexCredentialAdminError::RefreshUnavailable,
        RefreshFailure::Transport => CodexCredentialAdminError::RefreshAmbiguous,
    }
}

const fn map_identity_error(error: CodexIdentityVerificationError) -> CodexCredentialAdminError {
    match error {
        CodexIdentityVerificationError::Rejected => CodexCredentialAdminError::IdentityRejected,
        CodexIdentityVerificationError::Unavailable => {
            CodexCredentialAdminError::IdentityUnavailable
        }
    }
}

fn parse_import_document(
    payload: &Value,
) -> Result<Vec<ParsedCodexImportAccount>, CodexCredentialAdminError> {
    let shape = if looks_like_agent_identity_document(payload) {
        ImportDocumentShape::AgentIdentity
    } else if looks_like_credential_bundle(payload) {
        ImportDocumentShape::CredentialBundle
    } else if looks_like_auth_document(payload) {
        ImportDocumentShape::AuthDocument
    } else {
        ImportDocumentShape::Native
    };
    if shape == ImportDocumentShape::Native
        && payload.get("accounts").is_some()
        && payload.as_object().is_none_or(|object| {
            object
                .keys()
                .any(|key| !CPR_CONTAINER_KEYS.contains(&key.as_str()))
        })
    {
        return Err(CodexCredentialAdminError::InvalidInput);
    }
    let values = import_account_values(payload)?;
    let mut accounts = Vec::new();
    for value in values {
        let parsed = match shape {
            ImportDocumentShape::Native => Some(parse_native_account(value)?),
            ImportDocumentShape::AgentIdentity => Some(parse_agent_identity_account(value)?),
            ImportDocumentShape::CredentialBundle => parse_credential_bundle_account(value)?,
            ImportDocumentShape::AuthDocument => parse_auth_document_account(value)?,
        };
        if let Some(parsed) = parsed {
            accounts.push(parsed);
        }
    }
    Ok(accounts)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImportDocumentShape {
    Native,
    AgentIdentity,
    CredentialBundle,
    AuthDocument,
}

fn parse_agent_identity_account(
    value: &Value,
) -> Result<ParsedCodexImportAccount, CodexCredentialAdminError> {
    let account = value
        .as_object()
        .ok_or(CodexCredentialAdminError::InvalidInput)?;
    if account
        .get("platform")
        .and_then(Value::as_str)
        .is_some_and(|platform| !platform.eq_ignore_ascii_case("openai"))
        || account
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| !kind.eq_ignore_ascii_case("oauth"))
    {
        return Err(CodexCredentialAdminError::InvalidInput);
    }
    let identity = value
        .get("agent_identity")
        .or_else(|| value.get("credentials"))
        .unwrap_or(value);
    let auth_mode =
        first_string(value, &["auth_mode"]).or_else(|| first_string(identity, &["auth_mode"]));
    if auth_mode.as_deref() != Some("agentIdentity") {
        return Err(CodexCredentialAdminError::InvalidCredential);
    }
    let runtime_id = first_string(identity, &["agent_runtime_id"])
        .ok_or(CodexCredentialAdminError::InvalidCredential)?;
    let private_key = first_string(identity, &["agent_private_key"])
        .ok_or(CodexCredentialAdminError::InvalidCredential)?;
    Ok(ParsedCodexImportAccount {
        id: first_string(value, &["id"]),
        name: first_string(value, &["name", "label"]),
        email: first_string(identity, &["email"]),
        plan_type: first_string(identity, &["plan_type"]),
        chatgpt_account_id: first_string(identity, &["chatgpt_account_id", "account_id"]),
        chatgpt_user_id: first_string(identity, &["chatgpt_user_id"]),
        authentication: ParsedCodexAuthentication::AgentIdentity {
            runtime_id,
            private_key,
            task_id: first_string(identity, &["task_id"]),
        },
        status: first_string(value, &["status"]),
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

fn parse_native_account(
    value: &Value,
) -> Result<ParsedCodexImportAccount, CodexCredentialAdminError> {
    if value.get("accounts").is_some() {
        return Err(CodexCredentialAdminError::InvalidInput);
    }
    let account = value
        .as_object()
        .ok_or(CodexCredentialAdminError::InvalidInput)?;
    if account
        .keys()
        .any(|key| !CPR_ACCOUNT_KEYS.contains(&key.as_str()))
    {
        return Err(CodexCredentialAdminError::InvalidInput);
    }
    let access_token = first_string(value, &["token", "at", "accessToken", "access_token"]);
    let refresh_token = first_string(value, &["refreshToken", "refresh_token"]);
    if access_token.is_none() && refresh_token.is_none() {
        return Err(CodexCredentialAdminError::InvalidCredential);
    }
    Ok(ParsedCodexImportAccount {
        id: first_string(value, &["id"]),
        name: first_string(value, &["label", "email"]),
        email: first_string(value, &["email"]),
        plan_type: first_string(value, &["planType", "plan_type"]),
        chatgpt_account_id: first_string(value, &["accountId", "account_id"]),
        chatgpt_user_id: first_string(value, &["userId", "user_id"]),
        authentication: ParsedCodexAuthentication::OAuth {
            access_token,
            refresh_token,
            id_token: None,
        },
        status: first_string(value, &["status"]),
    })
}

fn parse_credential_bundle_account(
    value: &Value,
) -> Result<Option<ParsedCodexImportAccount>, CodexCredentialAdminError> {
    let account = value
        .as_object()
        .ok_or(CodexCredentialAdminError::InvalidInput)?;
    if let Some(credentials) = value.get("credentials") {
        if account
            .get("platform")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.eq_ignore_ascii_case("openai"))
            || account
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.eq_ignore_ascii_case("oauth"))
        {
            return Ok(None);
        }
        let access_token = first_path_string(
            credentials,
            &[
                &["access_token"],
                &["accessToken"],
                &["at"],
                &["token", "access_token"],
                &["token", "accessToken"],
                &["token", "at"],
            ],
        );
        let refresh_token = first_path_string(
            credentials,
            &[
                &["refresh_token"],
                &["refreshToken"],
                &["rt"],
                &["token", "refresh_token"],
                &["token", "refreshToken"],
                &["token", "rt"],
            ],
        );
        if access_token.is_none() && refresh_token.is_none() {
            return Ok(None);
        }
        return Ok(Some(ParsedCodexImportAccount {
            id: first_string(value, &["id"]),
            name: first_string(value, &["label", "name"]),
            email: first_path_string(credentials, &[&["email"], &["user", "email"]]),
            plan_type: first_path_string(
                credentials,
                &[&["plan_type"], &["planType"], &["plan", "type"]],
            ),
            chatgpt_account_id: first_path_string(
                credentials,
                &[
                    &["chatgpt_account_id"],
                    &["chatgptAccountId"],
                    &["account_id"],
                    &["accountId"],
                    &["account", "id"],
                    &["account", "account_id"],
                    &["account", "chatgpt_account_id"],
                ],
            ),
            chatgpt_user_id: first_path_string(
                credentials,
                &[
                    &["chatgpt_user_id"],
                    &["chatgptUserId"],
                    &["user_id"],
                    &["userId"],
                    &["user", "id"],
                    &["account", "user_id"],
                ],
            ),
            authentication: ParsedCodexAuthentication::OAuth {
                access_token,
                refresh_token,
                id_token: first_path_string(credentials, &[&["id_token"], &["idToken"]]),
            },
            status: first_string(value, &["status"]),
        }));
    }
    let access_token = first_path_string(
        value,
        &[
            &["tokens", "access_token"],
            &["tokens", "accessToken"],
            &["tokens", "at"],
            &["access_token"],
            &["accessToken"],
            &["token"],
            &["at"],
        ],
    );
    let refresh_token = first_path_string(
        value,
        &[
            &["tokens", "refresh_token"],
            &["tokens", "refreshToken"],
            &["tokens", "rt"],
            &["refresh_token"],
            &["refreshToken"],
            &["rt"],
        ],
    );
    if access_token.is_none() && refresh_token.is_none() {
        return Ok(None);
    }
    Ok(Some(ParsedCodexImportAccount {
        id: first_string(value, &["id"]),
        name: first_path_string(value, &[&["label"], &["name"], &["user", "name"]]),
        email: first_path_string(value, &[&["email"], &["user", "email"]]),
        plan_type: first_path_string(value, &[&["plan_type"], &["planType"], &["plan", "type"]]),
        chatgpt_account_id: first_path_string(
            value,
            &[
                &["chatgpt_account_id"],
                &["chatgptAccountId"],
                &["account_id"],
                &["accountId"],
                &["account", "id"],
                &["account", "account_id"],
                &["account", "chatgpt_account_id"],
            ],
        ),
        chatgpt_user_id: first_path_string(
            value,
            &[
                &["chatgpt_user_id"],
                &["chatgptUserId"],
                &["user_id"],
                &["userId"],
                &["user", "id"],
                &["account", "user_id"],
            ],
        ),
        authentication: ParsedCodexAuthentication::OAuth {
            access_token,
            refresh_token,
            id_token: first_path_string(
                value,
                &[&["tokens", "id_token"], &["id_token"], &["idToken"]],
            ),
        },
        status: first_string(value, &["status"]),
    }))
}

fn parse_auth_document_account(
    value: &Value,
) -> Result<Option<ParsedCodexImportAccount>, CodexCredentialAdminError> {
    let account = value
        .as_object()
        .ok_or(CodexCredentialAdminError::InvalidInput)?;
    if !auth_document_provider_is_openai(account) {
        return Ok(None);
    }
    let access_token = first_path_string(
        value,
        &[
            &["access_token"],
            &["accessToken"],
            &["at"],
            &["token", "access_token"],
            &["token", "accessToken"],
            &["token", "at"],
            &["token"],
        ],
    );
    let refresh_token = first_path_string(
        value,
        &[
            &["refresh_token"],
            &["refreshToken"],
            &["rt"],
            &["token", "refresh_token"],
            &["token", "refreshToken"],
            &["token", "rt"],
        ],
    );
    if access_token.is_none() && refresh_token.is_none() {
        return Ok(None);
    }
    Ok(Some(ParsedCodexImportAccount {
        id: None,
        name: first_string(value, &["label", "name", "email"]),
        email: first_string(value, &["email"]),
        plan_type: first_string(value, &["plan_type", "planType"]),
        chatgpt_account_id: first_string(
            value,
            &[
                "chatgpt_account_id",
                "chatgptAccountId",
                "account_id",
                "accountId",
            ],
        ),
        chatgpt_user_id: first_string(
            value,
            &["chatgpt_user_id", "chatgptUserId", "user_id", "userId"],
        ),
        authentication: ParsedCodexAuthentication::OAuth {
            access_token,
            refresh_token,
            id_token: first_string(value, &["id_token", "idToken"]),
        },
        status: account
            .get("disabled")
            .and_then(Value::as_bool)
            .filter(|disabled| *disabled)
            .map(|_| "disabled".to_owned()),
    }))
}

fn looks_like_credential_bundle(value: &Value) -> bool {
    value.get("exported_at").is_some()
        || value.get("proxies").is_some()
        || import_account_values(value).is_ok_and(|accounts| {
            accounts.iter().any(|account| {
                account.get("credentials").is_some()
                    || account.get("tokens").is_some()
                    || account.get("cachedQuota").is_some()
                    || account.get("cached_quota").is_some()
            })
        })
}

fn looks_like_agent_identity_document(value: &Value) -> bool {
    import_account_values(value).is_ok_and(|accounts| {
        accounts.iter().any(|account| {
            account
                .get("auth_mode")
                .and_then(Value::as_str)
                .is_some_and(|mode| mode == "agentIdentity")
                || account
                    .get("agent_identity")
                    .and_then(|identity| identity.get("agent_runtime_id"))
                    .is_some()
                || account
                    .get("credentials")
                    .and_then(|credentials| credentials.get("auth_mode"))
                    .and_then(Value::as_str)
                    .is_some_and(|mode| mode == "agentIdentity")
        })
    })
}

fn looks_like_auth_document(value: &Value) -> bool {
    import_account_values(value).is_ok_and(|accounts| {
        accounts.iter().any(|account| {
            account
                .as_object()
                .is_some_and(auth_document_provider_is_openai)
        })
    })
}

fn auth_document_provider_is_openai(account: &serde_json::Map<String, Value>) -> bool {
    account
        .get("type")
        .or_else(|| account.get("provider"))
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|provider| {
            provider.eq_ignore_ascii_case("openai")
                // CLIProxyAPI 的 OpenAI Codex OAuth 文件使用 `type: codex`。
                || provider.eq_ignore_ascii_case("codex")
        })
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn first_path_string(value: &Value, paths: &[&[&str]]) -> Option<String> {
    paths
        .iter()
        .find_map(|path| value_at_path(value, path).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn value_at_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn normalize_bearer(value: &str) -> String {
    let value = value.trim();
    value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .unwrap_or(value)
        .trim()
        .to_owned()
}

fn import_runtime_state(
    status: Option<&str>,
) -> Result<(bool, AccountAvailability), CodexCredentialAdminError> {
    match status
        .map(str::trim)
        .unwrap_or("active")
        .to_ascii_lowercase()
        .as_str()
    {
        "active" => Ok((true, AccountAvailability::Ready)),
        "disabled" => Ok((false, AccountAvailability::Ready)),
        "expired" => Ok((true, AccountAvailability::Expired)),
        "quota_exhausted" => Ok((true, AccountAvailability::QuotaExhausted)),
        "banned" => Ok((true, AccountAvailability::Banned)),
        _ => Err(CodexCredentialAdminError::InvalidInput),
    }
}
