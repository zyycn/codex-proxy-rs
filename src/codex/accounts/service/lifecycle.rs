use crate::codex::accounts::{model::AccountStatus, repository::AccountRepository};

use super::{
    pool_sync::pool_account_from_stored, AccountService, AccountServiceError, BatchDeleteAccounts,
    BatchUpdateAccountStatus, UpdateAccountStatus,
};

impl AccountService {
    pub async fn reset_usage(&self, account_id: &str) -> Result<bool, AccountServiceError> {
        match self.repository()?.exists(account_id).await {
            Ok(true) => {}
            Ok(false) => return Ok(false),
            Err(_) => return Err(AccountServiceError::Inspect),
        }
        self.usage_repository()?
            .reset_account(account_id)
            .await
            .map_err(|_| AccountServiceError::ResetUsage)?;
        self.account_pool.lock().await.reset_usage(account_id);
        Ok(true)
    }

    pub async fn update_label(
        &self,
        account_id: &str,
        label: Option<String>,
    ) -> Result<bool, AccountServiceError> {
        if label
            .as_ref()
            .is_some_and(|label| label.chars().count() > 64)
        {
            return Err(AccountServiceError::LabelTooLong);
        }
        match self
            .repository()?
            .set_label(account_id, label.clone())
            .await
        {
            Ok(true) => {
                self.account_pool.lock().await.set_label(account_id, label);
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(_) => Err(AccountServiceError::UpdateLabel),
        }
    }

    pub async fn update_status(
        &self,
        account_id: &str,
        status: &str,
    ) -> Result<Option<UpdateAccountStatus>, AccountServiceError> {
        let status = parse_admin_account_status(status)?;
        let repo = self.repository()?;
        match repo.set_status(account_id, status).await {
            Ok(true) => {
                self.sync_runtime_account_status(repo, account_id, status)
                    .await?;
                Ok(Some(UpdateAccountStatus {
                    id: account_id.to_string(),
                    status,
                }))
            }
            Ok(false) => Ok(None),
            Err(_) => Err(AccountServiceError::UpdateStatus),
        }
    }

    pub async fn delete(&self, account_id: &str) -> Result<bool, AccountServiceError> {
        match self.repository()?.delete(account_id).await {
            Ok(true) => {
                self.account_pool.lock().await.remove(account_id);
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(_) => Err(AccountServiceError::Delete),
        }
    }

    pub async fn batch_delete(
        &self,
        ids: Vec<String>,
    ) -> Result<BatchDeleteAccounts, AccountServiceError> {
        if ids.is_empty() {
            return Err(AccountServiceError::EmptyIds);
        }
        let repo = self.repository()?;
        let mut deleted = 0u32;
        let mut not_found = Vec::new();
        for account_id in ids {
            match repo.delete(&account_id).await {
                Ok(true) => {
                    self.account_pool.lock().await.remove(&account_id);
                    deleted += 1;
                }
                Ok(false) => not_found.push(account_id),
                Err(_) => return Err(AccountServiceError::Delete),
            }
        }
        Ok(BatchDeleteAccounts { deleted, not_found })
    }

    pub async fn batch_update_status(
        &self,
        ids: Vec<String>,
        status: &str,
    ) -> Result<BatchUpdateAccountStatus, AccountServiceError> {
        if ids.is_empty() {
            return Err(AccountServiceError::EmptyIds);
        }
        let status = parse_admin_account_status(status)?;
        let repo = self.repository()?;
        let mut updated = 0u32;
        let mut not_found = Vec::new();
        for account_id in ids {
            match repo.set_status(&account_id, status).await {
                Ok(true) => {
                    self.sync_runtime_account_status(repo, &account_id, status)
                        .await?;
                    updated += 1;
                }
                Ok(false) => not_found.push(account_id),
                Err(_) => return Err(AccountServiceError::UpdateStatus),
            }
        }
        Ok(BatchUpdateAccountStatus { updated, not_found })
    }

    async fn sync_runtime_account_status(
        &self,
        repo: &AccountRepository,
        account_id: &str,
        status: AccountStatus,
    ) -> Result<(), AccountServiceError> {
        if status == AccountStatus::Active {
            match repo.get(account_id).await {
                Ok(Some(account)) => {
                    self.account_pool
                        .lock()
                        .await
                        .insert(pool_account_from_stored(account));
                }
                Ok(None) => {
                    self.account_pool.lock().await.remove(account_id);
                }
                Err(_) => return Err(AccountServiceError::SyncStatus),
            }
            return Ok(());
        }
        self.account_pool.lock().await.remove(account_id);
        Ok(())
    }
}

pub(super) fn parse_admin_account_status(
    status: &str,
) -> Result<AccountStatus, AccountServiceError> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(AccountStatus::Active),
        "expired" => Ok(AccountStatus::Expired),
        "quota_exhausted" | "quota-exhausted" => Ok(AccountStatus::QuotaExhausted),
        "refreshing" => Ok(AccountStatus::Refreshing),
        "disabled" => Ok(AccountStatus::Disabled),
        "banned" => Ok(AccountStatus::Banned),
        other => Err(AccountServiceError::InvalidStatus(format!(
            "Unsupported account status: {other}"
        ))),
    }
}
