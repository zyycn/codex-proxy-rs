//! xAI 对中立 [`ProviderAccountStore`] 的明文 OAuth adapter。

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use gateway_core::engine::credential::{
    AccountAvailability, AccountStateChange, CredentialCasOutcome, CredentialCasUpdate,
    CredentialRevision, LoadedCredential, NewProviderAccount, OpaqueProviderData,
    PlaintextCredential, ProviderAccount, ProviderAccountId, ProviderAccountStore,
    ProviderAccountUpdate, QuotaObservation, QuotaWriteOutcome,
};
use gateway_core::error::StoreErrorKind;
use gateway_core::provider_ports::ProviderRefreshPolicy;
use gateway_core::routing::ProviderKind;
use serde_json::Value;
use thiserror::Error;

use super::types::{
    CreateGrokCredential, GrokAccountProfile, GrokCredentialAvailability, GrokCredentialRecord,
    GrokOAuthSecret, GrokOAuthSecretWire, OwnedGrokOAuthSecretWire, PreparedGrokCredentialRotation,
    RotateGrokCredential, RotateManagedGrokCredential, UpdateGrokCredentialState,
};
use crate::{GROK_CLI_BASE_URL, OFFICIAL_CLIENT_ID, SecretValue, VerifiedTokenSet};

const XAI_PROVIDER_KIND: &str = "xai";
const OAUTH_AUTH_METHOD: &str = "oauth";
const CREDENTIAL_SCHEMA_VERSION: u32 = 1;
const MAX_NAME_BYTES: usize = 512;
const MAX_IDENTITY_BYTES: usize = 2_048;
const MAX_REASON_BYTES: usize = 2_048;
const MAX_SECRET_BYTES: usize = 64 * 1_024;
const MAX_IMPORT_BATCH: usize = 200;

/// 已跨过官方 OAuth/OIDC 验证边界、等待转为 Core 创建命令的账号。
pub struct VerifiedGrokAccount {
    pub account_id: ProviderAccountId,
    pub name: String,
    pub email: Option<String>,
    pub upstream_account_id: Option<String>,
    pub plan_type: Option<String>,
    pub tokens: VerifiedTokenSet,
    pub enabled: bool,
}

impl std::fmt::Debug for VerifiedGrokAccount {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedGrokAccount")
            .field("account_id", &self.account_id)
            .field("name", &self.name)
            .field("email", &self.email.as_ref().map(|_| "[REDACTED]"))
            .field(
                "upstream_account_id",
                &self.upstream_account_id.as_ref().map(|_| "[REDACTED]"),
            )
            .field("plan_type", &self.plan_type)
            .field("tokens", &self.tokens)
            .field("enabled", &self.enabled)
            .finish()
    }
}

/// Provider-owned OAuth 账号明文导出；Debug 永不展开凭据。
pub struct GrokAccountExport(Value);

impl GrokAccountExport {
    #[must_use]
    pub fn into_value(self) -> Value {
        self.0
    }
}

impl std::fmt::Debug for GrokAccountExport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GrokAccountExport")
            .field(
                "account_count",
                &self
                    .0
                    .get("accounts")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len),
            )
            .field("credentials", &"[REDACTED]")
            .finish()
    }
}

/// 已从明文 Provider JSON 完整验证的 xAI account。
pub(crate) struct LoadedGrokCredential {
    pub(crate) account: ProviderAccount,
    pub(crate) access_token: SecretValue,
    pub(crate) refresh_token: SecretValue,
    pub(crate) id_token: Option<SecretValue>,
    pub(crate) scope: String,
    pub(crate) refresh_token_expires_at: Option<DateTime<Utc>>,
}

/// 管理端可读取的 xAI credential 生命周期事实；不包含任何 OAuth secret。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrokCredentialLifecycle {
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
    refresh_token_expires_at: Option<DateTime<Utc>>,
}

impl GrokCredentialLifecycle {
    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn credential_revision(&self) -> CredentialRevision {
        self.credential_revision
    }

    #[must_use]
    pub const fn refresh_token_expires_at(&self) -> Option<&DateTime<Utc>> {
        self.refresh_token_expires_at.as_ref()
    }
}

impl std::fmt::Debug for LoadedGrokCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoadedGrokCredential")
            .field("account", &self.account)
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("id_token", &self.id_token.as_ref().map(|_| "[REDACTED]"))
            .field("scope", &"[REDACTED]")
            .field("refresh_token_expires_at", &self.refresh_token_expires_at)
            .finish()
    }
}

/// 无状态的 xAI Admin command preparer；不读写持久层。
#[derive(Debug, Default, Clone, Copy)]
pub struct GrokCredentialAdmin;

impl GrokCredentialAdmin {
    /// 验证官方 OAuth account 并生成一次性 Core 创建命令。
    pub fn prepare_import(
        &self,
        input: &CreateGrokCredential,
    ) -> Result<NewProviderAccount, GrokCredentialRepositoryError> {
        validate_create(input)?;
        let revision = CredentialRevision::new(1)
            .map_err(|_| GrokCredentialRepositoryError::RevisionOverflow)?;
        let provider = ProviderKind::new(XAI_PROVIDER_KIND)
            .map_err(|_| GrokCredentialRepositoryError::InvalidInput("provider_kind"))?;
        let account = ProviderAccount::new(
            input.account_id.clone(),
            provider,
            input.name.clone(),
            input.account.subject.clone(),
            revision,
            to_system_time(input.account.access_token_expires_at),
        )
        .with_profile(
            input.account.email.clone(),
            input.account.upstream_account_id.clone(),
            input.account.plan_type.clone(),
        )
        .with_runtime_state(
            input.enabled,
            availability(input.initial_availability),
            input.initial_cooldown_until.map(to_system_time),
        )
        .with_refresh_schedule(true, Some(to_system_time(input.next_refresh_at)));
        Ok(NewProviderAccount {
            account,
            credential: encode_secret(&input.secret, &input.account)?,
        })
    }

    /// 把已验证 token 的 Provider 生命周期事实一次性投影为 Core 创建命令。
    pub fn prepare_verified_account(
        &self,
        input: &VerifiedGrokAccount,
        policy: ProviderRefreshPolicy,
    ) -> Result<NewProviderAccount, GrokCredentialRepositoryError> {
        let expires_in = input
            .tokens
            .expires_in()
            .ok_or(GrokCredentialRepositoryError::InvalidInput("expires_in"))?;
        let refresh_token = input
            .tokens
            .refresh_token()
            .ok_or(GrokCredentialRepositoryError::InvalidInput("refresh_token"))?;
        let now = SystemTime::now();
        let access_token_expires_at = now
            .checked_add(expires_in)
            .ok_or(GrokCredentialRepositoryError::InvalidInput("expires_in"))?;
        let next_refresh_at = policy
            .next_attempt_at(&input.account_id, access_token_expires_at, now)
            .map_err(|_| GrokCredentialRepositoryError::InvalidInput("next_refresh_at"))?;
        self.prepare_import(&CreateGrokCredential {
            account_id: input.account_id.clone(),
            name: input.name.clone(),
            secret: GrokOAuthSecret {
                access_token: input.tokens.access_token().clone(),
                refresh_token: refresh_token.clone(),
                id_token: input.tokens.id_token().cloned(),
                scope: input.tokens.scope().to_owned(),
            },
            account: GrokAccountProfile {
                subject: input.tokens.evidence().subject().to_owned(),
                email: input.email.clone(),
                upstream_account_id: input.upstream_account_id.clone(),
                plan_type: input.plan_type.clone(),
                access_token_expires_at: access_token_expires_at.into(),
                refresh_token_expires_at: None,
            },
            next_refresh_at: next_refresh_at.into(),
            enabled: input.enabled,
            initial_availability: GrokCredentialAvailability::Ready,
            initial_availability_reason: None,
            initial_cooldown_until: None,
        })
    }

    /// 将 Store 读取的 xAI 明文账号导出为规范 OAuth account bundle v1。
    pub fn export_oauth_bundle(
        &self,
        accounts: &[LoadedCredential],
        exported_at: DateTime<Utc>,
    ) -> Result<GrokAccountExport, GrokCredentialRepositoryError> {
        if accounts.len() > MAX_IMPORT_BATCH {
            return Err(GrokCredentialRepositoryError::InvalidInput("batch"));
        }
        let mut exported_accounts = Vec::with_capacity(accounts.len());
        for loaded in accounts {
            ensure_xai(&loaded.account)?;
            let mut secret = decode_secret(&loaded.credential)?;
            let mut credentials = serde_json::Map::new();
            credentials.insert(
                "access_token".to_owned(),
                Value::String(std::mem::take(&mut secret.access_token)),
            );
            credentials.insert(
                "refresh_token".to_owned(),
                Value::String(std::mem::take(&mut secret.refresh_token)),
            );
            if let Some(id_token) = secret.id_token.take() {
                credentials.insert("id_token".to_owned(), Value::String(id_token));
            }
            credentials.insert("token_type".to_owned(), Value::String("Bearer".to_owned()));
            credentials.insert(
                "expires_at".to_owned(),
                Value::String(
                    DateTime::<Utc>::from(loaded.account.access_token_expires_at()).to_rfc3339(),
                ),
            );
            credentials.insert(
                "base_url".to_owned(),
                Value::String(GROK_CLI_BASE_URL.to_owned()),
            );
            credentials.insert(
                "client_id".to_owned(),
                Value::String(OFFICIAL_CLIENT_ID.to_owned()),
            );
            credentials.insert(
                "scope".to_owned(),
                Value::String(std::mem::take(&mut secret.scope)),
            );
            if let Some(email) = loaded.account.email() {
                credentials.insert("email".to_owned(), Value::String(email.to_owned()));
            }
            exported_accounts.push(serde_json::json!({
                "name": loaded.account.name(),
                "platform": "grok",
                "type": "oauth",
                "credentials": credentials,
                "concurrency": 1,
                "priority": 1,
                "extra": loaded
                    .account
                    .email()
                    .map_or_else(|| serde_json::json!({}), |email| serde_json::json!({"email": email})),
            }));
        }
        Ok(GrokAccountExport(serde_json::json!({
            "version": 1,
            "type": "oauth-account-bundle",
            "exported_at": exported_at.to_rfc3339(),
            "accounts": exported_accounts,
            "proxies": [],
        })))
    }

    /// 验证身份不可重绑并生成完整 profile + credential CAS 命令。
    pub fn prepare_rotation(
        &self,
        input: &RotateManagedGrokCredential,
    ) -> Result<PreparedGrokCredentialRotation, GrokCredentialRepositoryError> {
        let account = &input.current.account;
        ensure_xai(account)?;
        prepare_rotation(
            account,
            &input.current.credential,
            account.revision(),
            &input.secret,
            &input.verified_account,
            input.next_refresh_at,
        )
    }
}

/// Provider 层只验证 xAI wire，并通过 Core port 持久化；这里没有 SQL 或加密。
#[derive(Clone)]
pub struct GrokCredentialRepository {
    store: Arc<dyn ProviderAccountStore>,
}

impl std::fmt::Debug for GrokCredentialRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GrokCredentialRepository")
            .field("store", &"[PROVIDER_ACCOUNT_STORE]")
            .finish()
    }
}

impl GrokCredentialRepository {
    #[must_use]
    pub fn new(store: Arc<dyn ProviderAccountStore>) -> Self {
        Self { store }
    }

    /// 读取不含 secret 的生命周期投影；Provider 仍是明文 credential JSON 的唯一解析者。
    pub async fn read_lifecycle(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<GrokCredentialLifecycle, GrokCredentialRepositoryError> {
        let loaded = self.load_current(account_id).await?;
        Ok(GrokCredentialLifecycle {
            account_id: loaded.account.id().clone(),
            credential_revision: loaded.account.revision(),
            refresh_token_expires_at: loaded.refresh_token_expires_at,
        })
    }

    /// Runtime refresh 用 credential revision CAS 原子轮换 OAuth token pair。
    pub(crate) async fn rotate_oauth_credential(
        &self,
        input: &RotateGrokCredential,
    ) -> Result<GrokCredentialRecord, GrokCredentialRepositoryError> {
        let current = self
            .store
            .load_credential(&input.account_id, input.expected_revision)
            .await
            .map_err(map_rotation_load_error)?;
        ensure_xai(&current.account)?;
        let prepared = prepare_rotation(
            &current.account,
            &current.credential,
            input.expected_revision,
            &input.secret,
            &input.verified_account,
            input.next_refresh_at,
        )?;
        let outcome = self
            .store
            .compare_and_swap_credential(prepared.credential)
            .await
            .map_err(map_store_error)?;
        let CredentialCasOutcome::Updated(revision) = outcome else {
            return Err(GrokCredentialRepositoryError::StaleCredentialRevision);
        };

        Ok(GrokCredentialRecord {
            account_id: input.account_id.clone(),
            credential_revision: revision,
        })
    }

    /// 临时失败时只推进持久刷新时刻，不改变 OAuth material 或账号可用性。
    pub(crate) async fn defer_refresh(
        &self,
        account_id: &ProviderAccountId,
        expected_revision: CredentialRevision,
        next_refresh_at: SystemTime,
    ) -> Result<(), GrokCredentialRepositoryError> {
        let current = self
            .store
            .load_credential(account_id, expected_revision)
            .await
            .map_err(map_rotation_load_error)?;
        ensure_xai(&current.account)?;
        let profile = ProviderAccountUpdate {
            account_id: account_id.clone(),
            name: current.account.name().to_owned(),
            email: current.account.email().map(str::to_owned),
            plan_type: current.account.plan_type().map(str::to_owned),
        };
        let update = CredentialCasUpdate::new(
            account_id.clone(),
            expected_revision,
            profile,
            current.credential,
            true,
            current.account.access_token_expires_at(),
            Some(next_refresh_at),
        )
        .map_err(|_| GrokCredentialRepositoryError::InvalidCredentialData)?;
        match self
            .store
            .compare_and_swap_credential(update)
            .await
            .map_err(map_store_error)?
        {
            CredentialCasOutcome::Updated(_) => Ok(()),
            CredentialCasOutcome::Conflict => {
                Err(GrokCredentialRepositoryError::StaleCredentialRevision)
            }
        }
    }

    /// 用同一 credential revision fence 更新可用性。
    pub async fn update_state(
        &self,
        input: &UpdateGrokCredentialState,
    ) -> Result<(), GrokCredentialRepositoryError> {
        validate_reason(input.availability_reason.as_deref())?;
        validate_cooldown(input.availability, input.cooldown_until)?;
        self.ensure_xai_account(&input.account_id).await?;
        self.store
            .apply_state_change(AccountStateChange {
                account_id: input.account_id.clone(),
                expected_revision: input.expected_revision,
                availability: availability(input.availability),
                reason: input.availability_reason.clone(),
                cooldown_until: input.cooldown_until.map(to_system_time),
                observed_at: to_system_time(input.observed_at),
            })
            .await
            .map_err(map_store_error)
    }

    /// 读取并完整校验一个 revision 对应的明文 OAuth credential。
    pub(crate) async fn load(
        &self,
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
    ) -> Result<LoadedGrokCredential, GrokCredentialRepositoryError> {
        let loaded = self
            .store
            .load_credential(account_id, revision)
            .await
            .map_err(map_store_error)?;
        loaded_from_core(loaded)
    }

    pub(crate) async fn load_managed(
        &self,
        account_id: &ProviderAccountId,
        revision: CredentialRevision,
    ) -> Result<LoadedCredential, GrokCredentialRepositoryError> {
        let loaded = self
            .store
            .load_credential(account_id, revision)
            .await
            .map_err(map_rotation_load_error)?;
        ensure_xai(&loaded.account)?;
        decode_secret(&loaded.credential)?;
        Ok(loaded)
    }

    /// 读取 xAI Provider 的全部账号；credential 逐行按 revision fence 加载。
    pub(crate) async fn list_loaded_for_provider(
        &self,
    ) -> Result<Vec<LoadedGrokCredential>, GrokCredentialRepositoryError> {
        let provider = ProviderKind::new(XAI_PROVIDER_KIND)
            .map_err(|_| GrokCredentialRepositoryError::InvalidCredentialData)?;
        let accounts = self
            .store
            .list_for_provider(&provider)
            .await
            .map_err(map_store_error)?;
        let mut loaded = Vec::with_capacity(accounts.len());
        for account in accounts {
            ensure_xai(&account)?;
            let credential = self.load(account.id(), account.revision()).await?;
            loaded.push(credential);
        }
        Ok(loaded)
    }

    pub(crate) async fn list_accounts_for_provider(
        &self,
    ) -> Result<Vec<ProviderAccount>, GrokCredentialRepositoryError> {
        let provider = ProviderKind::new(XAI_PROVIDER_KIND)
            .map_err(|_| GrokCredentialRepositoryError::InvalidCredentialData)?;
        let accounts = self
            .store
            .list_for_provider(&provider)
            .await
            .map_err(map_store_error)?;
        for account in &accounts {
            ensure_xai(account)?;
        }
        Ok(accounts)
    }

    pub(crate) async fn list_all_accounts(
        &self,
    ) -> Result<Vec<ProviderAccount>, GrokCredentialRepositoryError> {
        let accounts = self.store.list_accounts().await.map_err(map_store_error)?;
        Ok(accounts
            .into_iter()
            .filter(|account| account.provider().as_str() == XAI_PROVIDER_KIND)
            .collect())
    }

    pub(crate) async fn load_current(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<LoadedGrokCredential, GrokCredentialRepositoryError> {
        let account = self.ensure_xai_account(account_id).await?;
        self.load(account_id, account.revision()).await
    }

    pub(crate) async fn replace_quota(
        &self,
        account_id: ProviderAccountId,
        expected_revision: CredentialRevision,
        document: serde_json::Map<String, Value>,
        observed_at: SystemTime,
    ) -> Result<(), GrokCredentialRepositoryError> {
        let outcome = self
            .store
            .compare_and_swap_quota(QuotaObservation {
                account_id,
                expected_revision,
                quota: Some(OpaqueProviderData::new(document)),
                observed_at: Some(observed_at),
            })
            .await
            .map_err(map_store_error)?;
        match outcome {
            QuotaWriteOutcome::Updated => Ok(()),
            QuotaWriteOutcome::Conflict => {
                Err(GrokCredentialRepositoryError::StaleCredentialRevision)
            }
        }
    }

    pub(crate) async fn quota(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<Option<QuotaObservation>, GrokCredentialRepositoryError> {
        self.ensure_xai_account(account_id).await?;
        self.store
            .get_quotas(std::slice::from_ref(account_id))
            .await
            .map_err(map_store_error)
            .map(|mut observations| observations.pop())
    }

    pub(crate) async fn quota_observations(
        &self,
        accounts: &[ProviderAccount],
    ) -> Result<Vec<QuotaObservation>, GrokCredentialRepositoryError> {
        for account in accounts {
            ensure_xai(account)?;
        }
        let account_ids = accounts
            .iter()
            .map(|account| account.id().clone())
            .collect::<Vec<_>>();
        self.store
            .get_quotas(&account_ids)
            .await
            .map_err(map_store_error)
    }

    async fn ensure_xai_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<ProviderAccount, GrokCredentialRepositoryError> {
        let account = self
            .store
            .get_account(account_id)
            .await
            .map_err(map_store_error)?
            .ok_or(GrokCredentialRepositoryError::CredentialNotFound)?;
        ensure_xai(&account)?;
        Ok(account)
    }
}

pub(crate) fn loaded_from_core(
    loaded: LoadedCredential,
) -> Result<LoadedGrokCredential, GrokCredentialRepositoryError> {
    ensure_xai(&loaded.account)?;
    let mut wire = decode_secret(&loaded.credential)?;
    let access_token = SecretValue::new(std::mem::take(&mut wire.access_token));
    let refresh_token = SecretValue::new(std::mem::take(&mut wire.refresh_token));
    let id_token = wire.id_token.take().map(SecretValue::new);
    let scope = std::mem::take(&mut wire.scope);
    Ok(LoadedGrokCredential {
        account: loaded.account,
        access_token,
        refresh_token,
        id_token,
        scope,
        refresh_token_expires_at: wire.refresh_token_expires_at,
    })
}

fn encode_secret(
    secret: &GrokOAuthSecret,
    account: &GrokAccountProfile,
) -> Result<PlaintextCredential, GrokCredentialRepositoryError> {
    validate_secret(secret)?;
    let value = serde_json::to_value(GrokOAuthSecretWire {
        schema_version: CREDENTIAL_SCHEMA_VERSION,
        auth_method: OAUTH_AUTH_METHOD,
        access_token: secret.access_token.expose(),
        refresh_token: secret.refresh_token.expose(),
        id_token: secret.id_token.as_ref().map(SecretValue::expose),
        scope: &secret.scope,
        refresh_token_expires_at: account.refresh_token_expires_at,
    })
    .map_err(|_| GrokCredentialRepositoryError::InvalidCredentialData)?;
    let Value::Object(object) = value else {
        return Err(GrokCredentialRepositoryError::InvalidCredentialData);
    };
    if serde_json::to_vec(&object)
        .map_err(|_| GrokCredentialRepositoryError::InvalidCredentialData)?
        .len()
        > MAX_SECRET_BYTES
    {
        return Err(GrokCredentialRepositoryError::InvalidInput("secret"));
    }
    Ok(PlaintextCredential::new(object))
}

fn prepare_rotation(
    current: &ProviderAccount,
    current_credential: &PlaintextCredential,
    expected_revision: CredentialRevision,
    secret: &GrokOAuthSecret,
    verified_account: &GrokAccountProfile,
    next_refresh_at: DateTime<Utc>,
) -> Result<PreparedGrokCredentialRotation, GrokCredentialRepositoryError> {
    validate_profile(verified_account)?;
    validate_refresh_schedule(next_refresh_at, verified_account.access_token_expires_at)?;
    validate_secret(secret)?;
    ensure_xai(current)?;
    if current.revision() != expected_revision {
        return Err(GrokCredentialRepositoryError::StaleCredentialRevision);
    }
    if current.upstream_user_id() != verified_account.subject
        || current.upstream_account_id() != verified_account.upstream_account_id.as_deref()
    {
        return Err(GrokCredentialRepositoryError::IdentityRebind);
    }
    let profile = ProviderAccountUpdate {
        account_id: current.id().clone(),
        name: current.name().to_owned(),
        email: verified_account.email.clone(),
        plan_type: verified_account.plan_type.clone(),
    };
    let mut current_secret = decode_secret(current_credential)?;
    let merged_secret = GrokOAuthSecret {
        access_token: secret.access_token.clone(),
        refresh_token: secret.refresh_token.clone(),
        id_token: secret
            .id_token
            .clone()
            .or_else(|| current_secret.id_token.take().map(SecretValue::new)),
        scope: secret.scope.clone(),
    };
    let credential = CredentialCasUpdate::new(
        current.id().clone(),
        expected_revision,
        profile.clone(),
        encode_secret(&merged_secret, verified_account)?,
        true,
        to_system_time(verified_account.access_token_expires_at),
        Some(to_system_time(next_refresh_at)),
    )
    .map_err(|_| GrokCredentialRepositoryError::InvalidCredentialData)?;
    Ok(PreparedGrokCredentialRotation::new(profile, credential))
}

fn decode_secret(
    credential: &PlaintextCredential,
) -> Result<OwnedGrokOAuthSecretWire, GrokCredentialRepositoryError> {
    let value = Value::Object(credential.expose_to_provider().clone());
    let wire: OwnedGrokOAuthSecretWire = serde_json::from_value(value)
        .map_err(|_| GrokCredentialRepositoryError::InvalidCredentialData)?;
    if wire.schema_version != CREDENTIAL_SCHEMA_VERSION
        || wire.auth_method != OAUTH_AUTH_METHOD
        || wire.access_token.is_empty()
        || wire.refresh_token.is_empty()
        || validate_scope(&wire.scope).is_err()
    {
        return Err(GrokCredentialRepositoryError::InvalidCredentialData);
    }
    Ok(wire)
}

fn validate_create(input: &CreateGrokCredential) -> Result<(), GrokCredentialRepositoryError> {
    validate_name(&input.name)?;
    validate_profile(&input.account)?;
    validate_refresh_schedule(input.next_refresh_at, input.account.access_token_expires_at)?;
    validate_secret(&input.secret)?;
    validate_reason(input.initial_availability_reason.as_deref())?;
    validate_cooldown(input.initial_availability, input.initial_cooldown_until)
}

fn validate_profile(profile: &GrokAccountProfile) -> Result<(), GrokCredentialRepositoryError> {
    validate_identity(&profile.subject, "subject")?;
    if let Some(email) = profile.email.as_deref() {
        validate_identity(email, "email")?;
    }
    if let Some(account_id) = profile.upstream_account_id.as_deref() {
        validate_identity(account_id, "upstream_account_id")?;
    }
    let now = Utc::now();
    if profile.access_token_expires_at <= now
        || profile
            .refresh_token_expires_at
            .is_some_and(|expires_at| expires_at <= profile.access_token_expires_at)
    {
        return Err(GrokCredentialRepositoryError::InvalidInput(
            "token_lifetime",
        ));
    }
    Ok(())
}

fn validate_refresh_schedule(
    next_refresh_at: DateTime<Utc>,
    access_token_expires_at: DateTime<Utc>,
) -> Result<(), GrokCredentialRepositoryError> {
    if next_refresh_at >= access_token_expires_at {
        return Err(GrokCredentialRepositoryError::InvalidInput(
            "next_refresh_at",
        ));
    }
    Ok(())
}

fn validate_secret(secret: &GrokOAuthSecret) -> Result<(), GrokCredentialRepositoryError> {
    for value in [&secret.access_token, &secret.refresh_token] {
        let exposed = value.expose();
        if exposed.is_empty()
            || exposed.len() > MAX_SECRET_BYTES
            || exposed.chars().any(char::is_control)
        {
            return Err(GrokCredentialRepositoryError::InvalidInput("secret"));
        }
    }
    if secret.id_token.as_ref().is_some_and(|value| {
        let exposed = value.expose();
        exposed.is_empty()
            || exposed.len() > MAX_SECRET_BYTES
            || exposed.chars().any(char::is_control)
    }) {
        return Err(GrokCredentialRepositoryError::InvalidInput("secret"));
    }
    validate_scope(&secret.scope)?;
    Ok(())
}

fn validate_scope(scope: &str) -> Result<(), GrokCredentialRepositoryError> {
    let mut values = BTreeSet::new();
    if scope.is_empty()
        || scope.len() > 4 * 1_024
        || !scope.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
        || scope
            .split_ascii_whitespace()
            .any(|value| value.len() > 128 || !values.insert(value))
        || values.is_empty()
    {
        return Err(GrokCredentialRepositoryError::InvalidInput("scope"));
    }
    Ok(())
}

fn validate_name(value: &str) -> Result<(), GrokCredentialRepositoryError> {
    if value.trim().is_empty()
        || value.len() > MAX_NAME_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(GrokCredentialRepositoryError::InvalidInput("name"));
    }
    Ok(())
}

fn validate_identity(
    value: &str,
    field: &'static str,
) -> Result<(), GrokCredentialRepositoryError> {
    if value.trim().is_empty()
        || value.len() > MAX_IDENTITY_BYTES
        || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
    {
        return Err(GrokCredentialRepositoryError::InvalidInput(field));
    }
    Ok(())
}

fn validate_reason(reason: Option<&str>) -> Result<(), GrokCredentialRepositoryError> {
    if reason
        .is_some_and(|value| value.len() > MAX_REASON_BYTES || value.chars().any(char::is_control))
    {
        return Err(GrokCredentialRepositoryError::InvalidInput(
            "availability_reason",
        ));
    }
    Ok(())
}

fn validate_cooldown(
    availability: GrokCredentialAvailability,
    cooldown_until: Option<DateTime<Utc>>,
) -> Result<(), GrokCredentialRepositoryError> {
    if matches!(availability, GrokCredentialAvailability::Cooldown) != cooldown_until.is_some() {
        return Err(GrokCredentialRepositoryError::InvalidInput(
            "cooldown_until",
        ));
    }
    Ok(())
}

fn availability(value: GrokCredentialAvailability) -> AccountAvailability {
    match value {
        GrokCredentialAvailability::Unknown => AccountAvailability::Unknown,
        GrokCredentialAvailability::Ready => AccountAvailability::Ready,
        GrokCredentialAvailability::Cooldown => AccountAvailability::Cooldown,
        GrokCredentialAvailability::QuotaExhausted => AccountAvailability::QuotaExhausted,
        GrokCredentialAvailability::Expired => AccountAvailability::Expired,
        GrokCredentialAvailability::Banned => AccountAvailability::Banned,
        GrokCredentialAvailability::Invalid => AccountAvailability::Invalid,
    }
}

fn ensure_xai(account: &ProviderAccount) -> Result<(), GrokCredentialRepositoryError> {
    if account.provider().as_str() == XAI_PROVIDER_KIND {
        Ok(())
    } else {
        Err(GrokCredentialRepositoryError::WrongProviderKind)
    }
}

fn map_store_error(error: gateway_core::error::StoreError) -> GrokCredentialRepositoryError {
    match error.kind() {
        StoreErrorKind::Conflict => GrokCredentialRepositoryError::Conflict,
        StoreErrorKind::InvalidData | StoreErrorKind::InvalidState => {
            GrokCredentialRepositoryError::InvalidCredentialData
        }
        StoreErrorKind::Unavailable => GrokCredentialRepositoryError::Store,
        _ => GrokCredentialRepositoryError::Store,
    }
}

fn map_rotation_load_error(
    error: gateway_core::error::StoreError,
) -> GrokCredentialRepositoryError {
    if error.kind() == StoreErrorKind::Conflict {
        GrokCredentialRepositoryError::StaleCredentialRevision
    } else {
        map_store_error(error)
    }
}

fn to_system_time(value: DateTime<Utc>) -> SystemTime {
    value.into()
}

/// xAI Provider account adapter 的脱敏错误。
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum GrokCredentialRepositoryError {
    #[error("invalid xAI credential input: {0}")]
    InvalidInput(&'static str),
    #[error("xAI provider account was not found")]
    CredentialNotFound,
    #[error("provider account belongs to another Provider")]
    WrongProviderKind,
    #[error("credential revision is stale")]
    StaleCredentialRevision,
    #[error("verified xAI identity cannot be rebound")]
    IdentityRebind,
    #[error("xAI credential data is invalid")]
    InvalidCredentialData,
    #[error("xAI credential mutation conflicts with current state")]
    Conflict,
    #[error("credential revision overflow")]
    RevisionOverflow,
    #[error("provider account store is unavailable")]
    Store,
}
