use chrono::Utc;
use secrecy::ExposeSecret;

use crate::codex::accounts::{
    model::Account,
    pool::AccountCapacitySummary,
    repository::{AccountRepositoryResult, StoredAccount},
};

use super::AccountService;

impl AccountService {
    pub async fn reload_runtime_accounts_from_repository(&self) -> AccountRepositoryResult<usize> {
        let Some(repository) = self.repository.as_ref() else {
            return Ok(0);
        };
        let accounts = repository.list_pool_accounts().await?;
        let restored = accounts.len();
        let mut pool = self.account_pool.lock().await;
        for account in accounts {
            pool.insert(account);
        }
        Ok(restored)
    }

    pub async fn insert_runtime_account(&self, account: Account) {
        self.account_pool.lock().await.insert(account);
    }

    pub async fn acquire_runtime_account(&self, model: &str) -> Option<Account> {
        self.account_pool.lock().await.acquire(model)
    }

    pub async fn runtime_capacity_summary(&self) -> AccountCapacitySummary {
        self.account_pool.lock().await.capacity_summary(Utc::now())
    }
}

pub(super) fn pool_account_from_stored(account: StoredAccount) -> Account {
    Account {
        id: account.id,
        email: account.email,
        account_id: account.account_id,
        user_id: account.user_id,
        label: account.label,
        plan_type: account.plan_type,
        access_token: account.access_token.expose_secret().to_string(),
        refresh_token: account
            .refresh_token
            .as_ref()
            .map(|token| token.expose_secret().to_string()),
        access_token_expires_at: account.access_token_expires_at,
        status: account.status,
        quota_limit_reached: false,
        quota_cooldown_until: None,
        cloudflare_cooldown_until: None,
        request_count: 0,
        window_request_count: 0,
        window_started_at: None,
        window_reset_at: None,
        limit_window_seconds: None,
        added_at: account.added_at.to_rfc3339(),
        last_used_at: None,
    }
}
