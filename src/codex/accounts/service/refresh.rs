use secrecy::{ExposeSecret, SecretString};

use crate::{
    codex::accounts::{
        model::{Account, AccountStatus},
        repository::{AccountRepository, StoredAccount, TokenUpdate},
    },
    codex::oauth::RefreshFailure,
};

use super::{
    health::skipped_probe_result, runtime_pool::pool_account_from_stored, AccountProbeOutcome,
    AccountProbeResult, AccountService, RefreshAccountError,
};

impl AccountService {
    pub async fn refresh_account(
        &self,
        account_id: &str,
    ) -> Result<AccountProbeResult, RefreshAccountError> {
        let repo = self
            .repository
            .as_ref()
            .ok_or(RefreshAccountError::RepositoryUnavailable)?;
        let account = match repo.get(account_id).await {
            Ok(Some(account)) => account,
            Ok(None) => return Err(RefreshAccountError::NotFound),
            Err(_) => return Err(RefreshAccountError::Load),
        };
        if account.status == AccountStatus::Disabled {
            return Ok(skipped_probe_result(&account, "manually disabled"));
        }
        let Some(refresh_token) = account.refresh_token.as_ref() else {
            return Ok(skipped_probe_result(&account, "no refresh token"));
        };
        let Some(refresher) = self.token_refresher.as_ref() else {
            return Err(RefreshAccountError::TokenRefresherUnavailable);
        };

        let started_at = std::time::Instant::now();
        let previous_status = account.status;
        match refresher.refresh(refresh_token.expose_secret()).await {
            Ok(tokens) => {
                let updated = self
                    .persist_refreshed_account(
                        repo,
                        &account.id,
                        tokens.access_token,
                        tokens.refresh_token,
                    )
                    .await
                    .map_err(|()| RefreshAccountError::StoreRefreshed)?;
                Ok(AccountProbeResult {
                    id: updated.id,
                    email: updated.email,
                    previous_status,
                    outcome: AccountProbeOutcome::Alive,
                    status: Some(updated.status),
                    error: None,
                    duration_ms: Some(started_at.elapsed().as_millis()),
                })
            }
            Err(failure) => {
                let status = self
                    .apply_refresh_failure_status(repo, &account, failure)
                    .await;
                Ok(AccountProbeResult {
                    id: account.id,
                    email: account.email,
                    previous_status,
                    outcome: AccountProbeOutcome::Dead,
                    status,
                    error: Some(public_refresh_failure(failure).to_string()),
                    duration_ms: Some(started_at.elapsed().as_millis()),
                })
            }
        }
    }

    async fn persist_refreshed_account(
        &self,
        repo: &AccountRepository,
        account_id: &str,
        access_token: String,
        refresh_token: Option<String>,
    ) -> Result<Account, ()> {
        repo.update_tokens(
            account_id,
            TokenUpdate {
                access_token: SecretString::new(access_token.into()),
                refresh_token: refresh_token.map(|token| SecretString::new(token.into())),
                access_token_expires_at: None,
            },
        )
        .await
        .map_err(|_| ())?;
        let account = repo.get(account_id).await.map_err(|_| ())?.ok_or(())?;
        let account = pool_account_from_stored(account);
        self.account_pool.lock().await.insert(account.clone());
        Ok(account)
    }

    async fn apply_refresh_failure_status(
        &self,
        repo: &AccountRepository,
        account: &StoredAccount,
        failure: RefreshFailure,
    ) -> Option<AccountStatus> {
        let status = status_for_refresh_failure(failure)?;
        let _ = repo.set_status(&account.id, status).await;
        self.account_pool
            .lock()
            .await
            .set_status(&account.id, status);
        Some(status)
    }
}

fn status_for_refresh_failure(failure: RefreshFailure) -> Option<AccountStatus> {
    match failure {
        RefreshFailure::InvalidGrant => Some(AccountStatus::Expired),
        RefreshFailure::QuotaExhausted => Some(AccountStatus::QuotaExhausted),
        RefreshFailure::Banned => Some(AccountStatus::Banned),
        RefreshFailure::Disabled => Some(AccountStatus::Disabled),
        RefreshFailure::Transport => None,
    }
}

fn public_refresh_failure(failure: RefreshFailure) -> &'static str {
    match failure {
        RefreshFailure::InvalidGrant => "invalidGrant",
        RefreshFailure::QuotaExhausted => "quotaExhausted",
        RefreshFailure::Banned => "banned",
        RefreshFailure::Disabled => "disabled",
        RefreshFailure::Transport => "transport",
    }
}
