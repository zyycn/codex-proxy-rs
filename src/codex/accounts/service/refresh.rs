use std::sync::Arc;

use chrono::{Duration, Utc};
use secrecy::{ExposeSecret, SecretString};
use uuid::Uuid;

use crate::{
    codex::accounts::{
        model::{Account, AccountStatus},
        repository::{AccountRepository, StoredAccount, TokenUpdate},
    },
    codex::gateway::oauth::{RefreshFailure, TokenPair, TokenRefresher},
};

use super::{
    health::skipped_probe_result, pool_sync::pool_account_from_stored, AccountProbeOutcome,
    AccountProbeResult, AccountService, RefreshAccountError,
};

const REFRESH_LEASE_TTL_SECONDS: i64 = 5 * 60;

#[derive(Debug, Clone, Copy)]
enum RefreshFailurePolicy {
    PersistStatus,
    ReportOnly,
}

impl AccountService {
    pub async fn refresh_account(
        &self,
        account_id: &str,
    ) -> Result<AccountProbeResult, RefreshAccountError> {
        self.refresh_account_with_failure_policy(account_id, RefreshFailurePolicy::PersistStatus)
            .await
    }

    pub async fn probe_account_refresh(
        &self,
        account_id: &str,
    ) -> Result<AccountProbeResult, RefreshAccountError> {
        self.refresh_account_with_failure_policy(account_id, RefreshFailurePolicy::ReportOnly)
            .await
    }

    async fn refresh_account_with_failure_policy(
        &self,
        account_id: &str,
        failure_policy: RefreshFailurePolicy,
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
        if account.refresh_token.is_none() {
            return Ok(skipped_probe_result(&account, "no refresh token"));
        }
        let Some(refresher) = self.token_refresher.as_ref().cloned() else {
            return Err(RefreshAccountError::TokenRefresherUnavailable);
        };
        let lease_owner = refresh_lease_owner();
        let lease_until = Utc::now() + Duration::seconds(REFRESH_LEASE_TTL_SECONDS);
        match repo
            .try_acquire_refresh_lease(account_id, &lease_owner, lease_until)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return Ok(skipped_probe_result(&account, "refresh lease held"));
            }
            Err(_) => return Err(RefreshAccountError::LeaseAcquire),
        }

        let result = self
            .refresh_account_with_lease(repo, account_id, refresher, failure_policy)
            .await;
        if let Err(error) = repo.release_refresh_lease(account_id, &lease_owner).await {
            tracing::warn!(
                error = %error,
                account_id = %account_id,
                "释放 refresh lease 失败"
            );
        }
        result
    }

    async fn refresh_account_with_lease(
        &self,
        repo: &AccountRepository,
        account_id: &str,
        refresher: Arc<dyn TokenRefresher>,
        failure_policy: RefreshFailurePolicy,
    ) -> Result<AccountProbeResult, RefreshAccountError> {
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
        let started_at = std::time::Instant::now();
        let previous_status = account.status;
        match refresh_with_latest_disk_token_retry(repo, &account, refresh_token, &refresher).await
        {
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
                let status = match failure_policy {
                    RefreshFailurePolicy::PersistStatus => {
                        self.apply_refresh_failure_status(repo, &account, failure)
                            .await
                    }
                    RefreshFailurePolicy::ReportOnly => None,
                };
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
        self.websocket_pool.evict_account(account_id).await;
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
        self.websocket_pool.evict_account(&account.id).await;
        Some(status)
    }
}

async fn refresh_with_latest_disk_token_retry(
    repo: &AccountRepository,
    account: &StoredAccount,
    refresh_token: &SecretString,
    refresher: &Arc<dyn TokenRefresher>,
) -> Result<TokenPair, RefreshFailure> {
    let used_token = refresh_token.expose_secret().to_string();
    match refresher.refresh(&used_token).await {
        Err(RefreshFailure::InvalidGrant) => {
            if let Some(latest_token) = latest_disk_refresh_token(repo, account, &used_token).await
            {
                tracing::info!(
                    account_id = %account.id,
                    "检测到磁盘 refresh_token 已更新，使用最新值重试刷新"
                );
                return refresher.refresh(&latest_token).await;
            }
            Err(RefreshFailure::InvalidGrant)
        }
        result => result,
    }
}

async fn latest_disk_refresh_token(
    repo: &AccountRepository,
    account: &StoredAccount,
    used_token: &str,
) -> Option<String> {
    match repo.get(&account.id).await {
        Ok(Some(latest)) => latest
            .refresh_token
            .as_ref()
            .map(|token| token.expose_secret().to_string())
            .filter(|token| token != used_token),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                error = %error,
                account_id = %account.id,
                "刷新失败后重新读取磁盘 refresh_token 失败"
            );
            None
        }
    }
}

fn refresh_lease_owner() -> String {
    format!("{}:{}", std::process::id(), Uuid::new_v4())
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
