//! `ProviderAccountStore` 的 Codex 行转换；本文件不含 SQL。

use std::sync::Arc;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use gateway_core::engine::credential::{
    AccountAvailability, AccountStateChange, CredentialCasOutcome, CredentialCasUpdate,
    CredentialRevision, LoadedCredential, ProviderAccount, ProviderAccountId, ProviderAccountStore,
    ProviderAccountUpdate,
};
use gateway_core::routing::ProviderKind;
use secrecy::ExposeSecret;
use thiserror::Error;

use super::identity::CodexSignedIdentity;
use super::security::{CodexCredentialCodec, CodexCredentialDataError, CodexRuntimeCredential};
use super::types::{
    CodexAccountProfile, CodexCredentialData, CodexOAuthSecret, RotateCodexCredential,
};

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
        verify_identity(&current, &data, &input.verified_account)?;
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

    /// usage 暂时不可用时，只在签名 principal 与现有绑定完全一致后保存已轮换 token。
    pub async fn rotate_signed_secret(
        &self,
        account: &ProviderAccount,
        secret: CodexOAuthSecret,
        signed: &CodexSignedIdentity,
        next_refresh_at: SystemTime,
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
        if oauth.principal.oauth_subject != signed.oauth_subject()
            || oauth.principal.poid.as_deref() != signed.poid()
            || signed
                .claimed_account_id()
                .is_some_and(|value| account.upstream_account_id() != Some(value))
            || signed
                .claimed_user_id()
                .is_some_and(|value| account.upstream_user_id() != value)
        {
            return Err(CredentialRepositoryError::IdentityMismatch);
        }
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
            Some(SystemTime::from(signed.access_token_expires_at())),
            Some(next_refresh_at),
        )
        .map_err(|_| CredentialRepositoryError::InvalidCredentialData)?;
        cas_revision(self.store.compare_and_swap_credential(update).await?)
    }

    /// token 签名边界不可用时，以相同 credential 推进持久刷新退避。
    pub async fn defer_refresh(
        &self,
        account: &ProviderAccount,
        next_refresh_at: SystemTime,
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
        let update = CredentialCasUpdate::new(
            account.id().clone(),
            account.revision(),
            unchanged_profile(account),
            credential,
            has_refresh_token,
            account.access_token_expires_at(),
            Some(next_refresh_at),
        )
        .map_err(|_| CredentialRepositoryError::InvalidCredentialData)?;
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
        availability: AccountAvailability,
        reason: Option<String>,
        cooldown_until: Option<SystemTime>,
        observed_at: SystemTime,
    ) -> Result<(), CredentialRepositoryError> {
        self.store
            .apply_state_change(AccountStateChange {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                availability,
                reason,
                cooldown_until,
                observed_at,
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

fn verify_identity(
    current: &LoadedCredential,
    credential: &CodexCredentialData,
    verified: &CodexAccountProfile,
) -> Result<(), CredentialRepositoryError> {
    if current.account.provider().as_str() != PROVIDER_NAME
        || current.account.upstream_account_id() != Some(verified.chatgpt_account_id.as_str())
        || verified.chatgpt_user_id != current.account.upstream_user_id()
        || credential.oauth().is_none_or(|credential| {
            verified.oauth_subject != credential.principal.oauth_subject
                || verified.poid != credential.principal.poid
        })
    {
        return Err(CredentialRepositoryError::IdentityMismatch);
    }
    Ok(())
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
    #[error("Codex credential identity does not match")]
    IdentityMismatch,
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
