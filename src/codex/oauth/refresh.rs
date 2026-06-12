use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use thiserror::Error;
use tokio::sync::Semaphore;

use crate::codex::accounts::model::{Account, AccountStatus};

use super::token::TokenPair;

#[derive(Debug, Clone, Copy)]
pub struct RefreshPolicy {
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshTrigger {
    BeforeExpiry,
    Unauthorized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RefreshFailure {
    #[error("refresh token is invalid or expired")]
    InvalidGrant,
    #[error("account quota is exhausted")]
    QuotaExhausted,
    #[error("account is banned")]
    Banned,
    #[error("account is disabled")]
    Disabled,
    #[error("refresh transport failed")]
    Transport,
}

#[derive(Debug, Error)]
pub enum RefreshError {
    #[error("refresh task semaphore closed")]
    ConcurrencyClosed,
    #[error("refresh transport failed")]
    Transport,
}

#[async_trait]
pub trait TokenRefresher: Send + Sync + 'static {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure>;
}

#[derive(Clone)]
pub struct RefreshScheduler<C> {
    policy: RefreshPolicy,
    client: Arc<C>,
    semaphore: Arc<Semaphore>,
}

impl<C> RefreshScheduler<C>
where
    C: TokenRefresher,
{
    pub fn new(policy: RefreshPolicy, client: C) -> Self {
        let concurrency = policy.refresh_concurrency.max(1) as usize;
        Self {
            policy,
            client: Arc::new(client),
            semaphore: Arc::new(Semaphore::new(concurrency)),
        }
    }

    pub async fn refresh_account_at(
        &self,
        account: &Account,
        trigger: RefreshTrigger,
        now: DateTime<Utc>,
    ) -> Result<Account, RefreshError> {
        if !self.should_refresh(account, trigger, now) {
            return Ok(account.clone());
        }
        let Some(refresh_token) = account.refresh_token.as_deref() else {
            let mut expired = account.clone();
            expired.status = AccountStatus::Expired;
            return Ok(expired);
        };

        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| RefreshError::ConcurrencyClosed)?;
        // 刷新响应未返回 refresh_token 时必须保留旧值，避免把账号永久刷新能力清空。
        match self.client.refresh(refresh_token).await {
            Ok(token_pair) => Ok(apply_token_pair(account, token_pair)),
            Err(RefreshFailure::Transport) => Err(RefreshError::Transport),
            Err(error) => Ok(apply_refresh_failure(account, error)),
        }
    }

    fn should_refresh(
        &self,
        account: &Account,
        trigger: RefreshTrigger,
        now: DateTime<Utc>,
    ) -> bool {
        if account.status != AccountStatus::Active {
            return false;
        }
        match trigger {
            RefreshTrigger::Unauthorized => true,
            RefreshTrigger::BeforeExpiry => account
                .access_token_expires_at
                .is_some_and(|expires_at| expires_at <= now + self.refresh_margin()),
        }
    }

    fn refresh_margin(&self) -> Duration {
        let seconds = self.policy.refresh_margin_seconds.min(86_400 * 7) as i64;
        Duration::seconds(seconds)
    }
}

fn apply_token_pair(account: &Account, token_pair: TokenPair) -> Account {
    let mut refreshed = account.clone();
    refreshed.access_token = token_pair.access_token;
    if let Some(refresh_token) = token_pair.refresh_token {
        refreshed.refresh_token = Some(refresh_token);
    }
    refreshed.status = AccountStatus::Active;
    refreshed
}

fn apply_refresh_failure(account: &Account, failure: RefreshFailure) -> Account {
    let mut updated = account.clone();
    updated.status = match failure {
        RefreshFailure::InvalidGrant => AccountStatus::Expired,
        RefreshFailure::QuotaExhausted => AccountStatus::QuotaExhausted,
        RefreshFailure::Banned => AccountStatus::Banned,
        RefreshFailure::Disabled => AccountStatus::Disabled,
        RefreshFailure::Transport => AccountStatus::Active,
    };
    updated
}
