//! `ProviderAccountStore` 的 Codex 行转换；本文件不含 SQL

use std::sync::Arc;
use std::time::SystemTime;

use gateway_core::error::ErrorSource;

use gateway_core::account::{
    AccountErrorReason, AccountStateChange, CredentialCasOutcome, CredentialCasUpdate,
    CredentialRevision, CredentialState, LoadedCredential, ProviderAccount, ProviderAccountId,
    ProviderAccountStore, ProviderAccountUpdate, ProviderRefreshQuery,
};
use gateway_core::routing::ProviderKind;
use secrecy::ExposeSecret;
use thiserror::Error;

use super::codec::{CodexCredentialCodec, CodexCredentialDataError, CodexRuntimeCredential};
use super::types::{CodexCredentialData, CodexOAuthSecret};

const PROVIDER_NAME: &str = "openai";

#[derive(Clone)]
pub struct CodexCredentialRepository {
    store: Arc<dyn ProviderAccountStore>,
}

impl CodexCredentialRepository {
    #[must_use]
    pub const fn new(store: Arc<dyn ProviderAccountStore>) -> Self {
        Self { store }
    }

    #[must_use]
    pub fn store(&self) -> &Arc<dyn ProviderAccountStore> {
        &self.store
    }

    /// 持久化成功 RT exchange 的 token，同时保留既有账号身份投影
    ///
    /// Refresh endpoint 已经是这次 AT/RT 轮换的授权边界；这里仅以 revision
    /// CAS 保护并发写入，不重新验证新 access token 的身份声明
    pub async fn rotate_refreshed_oauth_secret(
        &self,
        account: &ProviderAccount,
        secret: CodexOAuthSecret,
        access_token_expires_at: Option<SystemTime>,
        next_refresh_at: Option<SystemTime>,
    ) -> Result<CredentialRevision, CredentialRepositoryError> {
        let current = self
            .store
            .load_credential(account.id(), account.revision())
            .await?;
        // load_credential 已校验凭据版本；套餐与额度可在 RT exchange 期间更新，
        // 这些运行时事实变化不能使已成功轮换的 token 丢失
        let mut data = CodexCredentialCodec::decode_complete(&current.credential)?;
        let oauth = data
            .oauth_mut()
            .ok_or(CredentialRepositoryError::InvalidCredentialData(None))?;
        oauth.access_token = secret.access_token.expose_secret().to_owned();
        oauth.refresh_token = secret
            .refresh_token
            .as_ref()
            .map(|value| value.expose_secret().to_owned());
        oauth.id_token = secret
            .id_token
            .as_ref()
            .map(|value| value.expose_secret().to_owned());
        let credential = CodexCredentialCodec::encode_complete(data)?;
        let update = CredentialCasUpdate::new(
            account.id().clone(),
            account.revision(),
            unchanged_profile(account),
            credential,
            secret.refresh_token.is_some(),
            access_token_expires_at,
            next_refresh_at,
        )
        .map_err(|source| {
            CredentialRepositoryError::InvalidCredentialData(Some(ErrorSource::new(source)))
        })?
        .preserving_profile()
        .with_account_state(
            oauth_account_state(account, CredentialState::Ready),
            SystemTime::now(),
            None,
            None,
        );
        cas_revision(self.store.compare_and_swap_credential(update).await?)
    }

    /// 以相同 credential 原子推进刷新退避及本次上游错误事实
    pub async fn defer_refresh(
        &self,
        account: &ProviderAccount,
        next_refresh_at: SystemTime,
        error_reason: Option<AccountErrorReason>,
        message: Option<String>,
    ) -> Result<CredentialRevision, CredentialRepositoryError> {
        let current = self
            .store
            .load_credential(account.id(), account.revision())
            .await?;
        let data = CodexCredentialCodec::decode_complete(&current.credential)?;
        let has_refresh_token = data.has_refresh_token();
        let credential = CodexCredentialCodec::encode_complete(data)?;
        let mut update = CredentialCasUpdate::new(
            account.id().clone(),
            account.revision(),
            unchanged_profile(account),
            credential,
            has_refresh_token,
            account.access_token_expires_at(),
            Some(next_refresh_at),
        )
        .map_err(|source| {
            CredentialRepositoryError::InvalidCredentialData(Some(ErrorSource::new(source)))
        })?
        .preserving_profile();
        if let Some(error_reason) = error_reason {
            update = update.with_account_state(
                account.credential_state(),
                SystemTime::now(),
                Some(error_reason),
                message,
            );
        }
        cas_revision(self.store.compare_and_swap_credential(update).await?)
    }

    pub async fn list_for_provider(
        &self,
    ) -> Result<Vec<ProviderAccount>, CredentialRepositoryError> {
        let provider = ProviderKind::new(PROVIDER_NAME).map_err(|source| {
            CredentialRepositoryError::InvalidCredentialData(Some(ErrorSource::new(source)))
        })?;
        self.store
            .list_for_provider(&provider)
            .await
            .map_err(Into::into)
    }

    pub(crate) async fn list_for_accounts(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<Vec<ProviderAccount>, CredentialRepositoryError> {
        let provider = ProviderKind::new(PROVIDER_NAME).map_err(|source| {
            CredentialRepositoryError::InvalidCredentialData(Some(ErrorSource::new(source)))
        })?;
        self.store
            .list_for_provider_accounts(&provider, accounts)
            .await
            .map_err(Into::into)
    }

    pub async fn list_refresh_candidates(
        &self,
        query: ProviderRefreshQuery,
    ) -> Result<Vec<LoadedCredential>, CredentialRepositoryError> {
        self.store
            .list_refresh_candidates(query)
            .await
            .map_err(Into::into)
    }

    pub fn decode_runtime_credential(
        &self,
        loaded: &LoadedCredential,
    ) -> Result<CodexRuntimeCredential, CredentialRepositoryError> {
        if loaded.account.provider().as_str() != PROVIDER_NAME {
            return Err(CredentialRepositoryError::InvalidCredentialData(None));
        }
        let data = CodexCredentialCodec::decode_complete(&loaded.credential)?;
        if data.authentication_kind() != loaded.account.authentication_kind() {
            return Err(CredentialRepositoryError::InvalidCredentialData(None));
        }
        CodexCredentialCodec::decode(&loaded.credential).map_err(Into::into)
    }

    pub async fn load_runtime_credential(
        &self,
        account: &ProviderAccount,
    ) -> Result<CodexRuntimeCredential, CredentialRepositoryError> {
        if account.provider().as_str() != PROVIDER_NAME {
            return Err(CredentialRepositoryError::InvalidCredentialData(None));
        }
        let loaded = self
            .store
            .load_credential(account.id(), account.revision())
            .await?;
        if loaded.account != *account {
            return Err(CredentialRepositoryError::RevisionConflict(None));
        }
        self.decode_runtime_credential(&loaded)
    }

    /// 选号专用的凭据重读，返回最新账号快照
    ///
    /// 成功推理会持续刷新额度观测时刻；未耗尽账号的访问结论不变时调度资格不变，
    /// 沿用最新快照，避免繁忙账号在选号阶段反复冲突
    pub async fn reload_selected_credential(
        &self,
        account: &ProviderAccount,
    ) -> Result<(ProviderAccount, CodexRuntimeCredential), CredentialRepositoryError> {
        if account.provider().as_str() != PROVIDER_NAME {
            return Err(CredentialRepositoryError::InvalidCredentialData(None));
        }
        let loaded = self
            .store
            .load_credential(account.id(), account.revision())
            .await?;
        if !only_quota_observation_advanced(account, &loaded.account) {
            return Err(CredentialRepositoryError::RevisionConflict(None));
        }
        let runtime = self.decode_runtime_credential(&loaded)?;
        Ok((loaded.account, runtime))
    }

    pub async fn load_complete_data(
        &self,
        account: &ProviderAccount,
    ) -> Result<CodexCredentialData, CredentialRepositoryError> {
        let loaded = self
            .store
            .load_credential(account.id(), account.revision())
            .await?;
        CodexCredentialCodec::decode_complete(&loaded.credential).map_err(Into::into)
    }

    pub async fn compare_and_swap_data(
        &self,
        account: &ProviderAccount,
        data: CodexCredentialData,
    ) -> Result<CredentialRevision, CredentialRepositoryError> {
        let has_refresh_token = data.has_refresh_token();
        let credential = CodexCredentialCodec::encode_complete(data)?;
        let update = CredentialCasUpdate::new(
            account.id().clone(),
            account.revision(),
            ProviderAccountUpdate {
                account_id: account.id().clone(),
                name: account.name().to_owned(),
                email: account.email().map(str::to_owned),
                plan_type: account.plan_type().map(str::to_owned),
            },
            credential,
            has_refresh_token,
            account.access_token_expires_at(),
            account.next_refresh_at(),
        )
        .map_err(|source| {
            CredentialRepositoryError::InvalidCredentialData(Some(ErrorSource::new(source)))
        })?
        .preserving_profile();
        cas_revision(self.store.compare_and_swap_credential(update).await?)
    }

    pub async fn apply_state(
        &self,
        account: &ProviderAccount,
        credential_state: CredentialState,
        observed_at: SystemTime,
    ) -> Result<(), CredentialRepositoryError> {
        self.apply_state_with_reason(
            account,
            credential_state,
            observed_at,
            credential_state.error_reason(),
            None,
        )
        .await
    }

    /// 写入凭据事实及其稳定错误原因；额度事实不经过此入口
    pub async fn apply_state_with_reason(
        &self,
        account: &ProviderAccount,
        credential_state: CredentialState,
        observed_at: SystemTime,
        error_reason: Option<AccountErrorReason>,
        message: Option<String>,
    ) -> Result<(), CredentialRepositoryError> {
        let credential_state = oauth_account_state(account, credential_state);
        let message = message.filter(|value| !value.trim().is_empty());
        let error_reason = if credential_state == CredentialState::Ready {
            message.as_ref().and(error_reason)
        } else {
            error_reason.or_else(|| credential_state.error_reason())
        };
        self.store
            .apply_state_change(AccountStateChange {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                credential_state,
                observed_at,
                error_reason,
                message,
            })
            .await?;
        Ok(())
    }
}

fn unchanged_profile(account: &ProviderAccount) -> ProviderAccountUpdate {
    ProviderAccountUpdate {
        account_id: account.id().clone(),
        name: account.name().to_owned(),
        email: account.email().map(str::to_owned),
        plan_type: account.plan_type().map(str::to_owned),
    }
}

fn cas_revision(
    outcome: CredentialCasOutcome,
) -> Result<CredentialRevision, CredentialRepositoryError> {
    match outcome {
        CredentialCasOutcome::Updated(revision) => Ok(revision),
        CredentialCasOutcome::Conflict => Err(CredentialRepositoryError::RevisionConflict(None)),
    }
}

#[derive(Debug, Error)]
pub enum CredentialRepositoryError {
    #[error("Codex credential data is invalid")]
    InvalidCredentialData(#[source] Option<ErrorSource>),
    #[error("Codex credential revision conflict")]
    RevisionConflict(#[source] Option<ErrorSource>),
    #[error("provider account store is unavailable")]
    Store(#[source] gateway_core::error::StoreError),
}

impl From<gateway_core::error::StoreError> for CredentialRepositoryError {
    fn from(error: gateway_core::error::StoreError) -> Self {
        match error.kind() {
            gateway_core::error::StoreErrorKind::Conflict => {
                Self::RevisionConflict(Some(ErrorSource::new(error)))
            }
            _ => Self::Store(error),
        }
    }
}

impl From<CodexCredentialDataError> for CredentialRepositoryError {
    fn from(error: CodexCredentialDataError) -> Self {
        Self::InvalidCredentialData(Some(ErrorSource::new(error)))
    }
}

/// OAuth 未取得用户身份时保留未验证状态；通用账号层不解释认证类型
fn oauth_account_state(account: &ProviderAccount, observed: CredentialState) -> CredentialState {
    if account.authentication_kind() == super::CODEX_AUTHENTICATION_KIND_OAUTH
        && account.upstream_user_id().is_none()
    {
        CredentialState::Unknown
    } else {
        observed
    }
}

fn only_quota_observation_advanced(selected: &ProviderAccount, current: &ProviderAccount) -> bool {
    if selected == current {
        return true;
    }
    let (before, after) = (selected.quota(), current.quota());
    // 耗尽状态的复核时间依赖观测时刻，必须按冲突处理
    if before.is_exhausted()
        || before.access() != after.access()
        || before.evidence() != after.evidence()
        || before.reset_at() != after.reset_at()
    {
        return false;
    }
    // 只替换额度观测，其余账号事实必须与选号时完全一致
    let normalized = current.clone().with_account_facts(
        current.enabled(),
        current.credential_state(),
        before,
        current.last_error_reason(),
        current.last_error_message().map(str::to_owned),
    );
    normalized == *selected
}
