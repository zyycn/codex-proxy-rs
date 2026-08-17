//! `ProviderAccountStore` 的 Codex 行转换；本文件不含 SQL。

use std::sync::Arc;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use gateway_core::engine::credential::{
    AccountErrorReason, AccountStateChange, CredentialCasOutcome, CredentialCasUpdate,
    CredentialRevision, CredentialState, ProviderAccount, ProviderAccountId, ProviderAccountStore,
    ProviderAccountUpdate,
};
use gateway_core::routing::ProviderKind;
use secrecy::ExposeSecret;
use thiserror::Error;

use super::security::{CodexCredentialCodec, CodexCredentialDataError, CodexRuntimeCredential};
use super::types::{CodexCredentialData, CodexOAuthSecret, RotateCodexCredential};

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

    pub async fn rotate_oauth_secret(
        &self,
        input: RotateCodexCredential,
    ) -> Result<CredentialRevision, CredentialRepositoryError> {
        let account_id = ProviderAccountId::new(input.account_id)
            .map_err(|_| CredentialRepositoryError::InvalidInput("account_id"))?;
        let expected = CredentialRevision::new(input.expected_credential_revision)
            .map_err(|_| CredentialRepositoryError::InvalidInput("credential_revision"))?;
        let current = self.store.load_credential(&account_id, expected).await?;
        let mut data = CodexCredentialCodec::decode_complete(&current.credential)?;
        let oauth = data
            .oauth_mut()
            .ok_or(CredentialRepositoryError::InvalidCredentialData)?;
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
        let credential = CodexCredentialCodec::encode_complete(data)?;
        let update = CredentialCasUpdate::new(
            account_id.clone(),
            expected,
            ProviderAccountUpdate {
                account_id,
                name: current.account.name().to_owned(),
                email: input.verified_account.email.clone(),
                plan_type: input.verified_account.plan_type.clone(),
            },
            credential,
            input.secret.refresh_token.is_some(),
            Some(required_time(
                input.verified_account.access_token_expires_at,
            )?),
            optional_time(input.next_refresh_at),
        )
        .map_err(|_| CredentialRepositoryError::InvalidCredentialData)?;
        cas_revision(self.store.compare_and_swap_credential(update).await?)
    }

    /// 持久化成功 RT exchange 的 token，同时保留既有账号身份投影。
    ///
    /// Refresh endpoint 已经是这次 AT/RT 轮换的授权边界；这里仅以 revision
    /// CAS 保护并发写入，不重新验证新 access token 的身份声明。
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
        if current.account != *account {
            return Err(CredentialRepositoryError::RevisionConflict);
        }
        let mut data = CodexCredentialCodec::decode_complete(&current.credential)?;
        let oauth = data
            .oauth_mut()
            .ok_or(CredentialRepositoryError::InvalidCredentialData)?;
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
        .map_err(|_| CredentialRepositoryError::InvalidCredentialData)?
        .with_account_state(CredentialState::Ready, SystemTime::now(), None, None);
        cas_revision(self.store.compare_and_swap_credential(update).await?)
    }

    /// 以相同 credential 原子推进刷新退避及本次上游错误事实。
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
        if current.account != *account {
            return Err(CredentialRepositoryError::RevisionConflict);
        }
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
        .map_err(|_| CredentialRepositoryError::InvalidCredentialData)?;
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
        let provider = ProviderKind::new(PROVIDER_NAME)
            .map_err(|_| CredentialRepositoryError::InvalidCredentialData)?;
        self.store
            .list_for_provider(&provider)
            .await
            .map_err(Into::into)
    }

    pub async fn load_runtime_credential(
        &self,
        account: &ProviderAccount,
    ) -> Result<CodexRuntimeCredential, CredentialRepositoryError> {
        if account.provider().as_str() != PROVIDER_NAME {
            return Err(CredentialRepositoryError::InvalidCredentialData);
        }
        let loaded = self
            .store
            .load_credential(account.id(), account.revision())
            .await?;
        if loaded.account != *account {
            return Err(CredentialRepositoryError::RevisionConflict);
        }
        CodexCredentialCodec::decode(&loaded.credential).map_err(Into::into)
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
        .map_err(|_| CredentialRepositoryError::InvalidCredentialData)?;
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

    /// 写入凭据事实及其稳定错误原因；额度事实不经过此入口。
    pub async fn apply_state_with_reason(
        &self,
        account: &ProviderAccount,
        credential_state: CredentialState,
        observed_at: SystemTime,
        error_reason: Option<AccountErrorReason>,
        message: Option<String>,
    ) -> Result<(), CredentialRepositoryError> {
        if !account.enabled() {
            return Ok(());
        }
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
        CredentialCasOutcome::Conflict => Err(CredentialRepositoryError::RevisionConflict),
    }
}

fn required_time(value: Option<DateTime<Utc>>) -> Result<SystemTime, CredentialRepositoryError> {
    value
        .map(SystemTime::from)
        .ok_or(CredentialRepositoryError::InvalidCredentialData)
}

fn optional_time(value: Option<DateTime<Utc>>) -> Option<SystemTime> {
    value.map(SystemTime::from)
}

#[derive(Debug, Error)]
pub enum CredentialRepositoryError {
    #[error("invalid Codex credential input: {0}")]
    InvalidInput(&'static str),
    #[error("Codex credential data is invalid")]
    InvalidCredentialData,
    #[error("Codex credential revision conflict")]
    RevisionConflict,
    #[error("provider account store is unavailable")]
    Store,
}

impl From<gateway_core::error::StoreError> for CredentialRepositoryError {
    fn from(error: gateway_core::error::StoreError) -> Self {
        match error.kind() {
            gateway_core::error::StoreErrorKind::Conflict => Self::RevisionConflict,
            gateway_core::error::StoreErrorKind::Unavailable
            | gateway_core::error::StoreErrorKind::InvalidState
            | gateway_core::error::StoreErrorKind::InvalidData => Self::Store,
            _ => Self::Store,
        }
    }
}

impl From<CodexCredentialDataError> for CredentialRepositoryError {
    fn from(_: CodexCredentialDataError) -> Self {
        Self::InvalidCredentialData
    }
}
