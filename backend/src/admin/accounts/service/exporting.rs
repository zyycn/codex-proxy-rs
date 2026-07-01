use secrecy::ExposeSecret;

use super::{
    types::{ExportedAccount, ExportedAccounts},
    AdminAccountError, AdminAccountService,
};
use crate::{
    infra::time::china_rfc3339_str,
    upstream::accounts::{model::AccountStatus, store::StoredAccount},
};

const EXPORT_PAGE_LIMIT: u32 = 200;

impl AdminAccountService {
    pub async fn export(&self, ids: Vec<String>) -> Result<ExportedAccounts, AdminAccountError> {
        let accounts = if ids.is_empty() {
            self.export_all_accounts().await?
        } else {
            self.export_accounts_by_id(ids).await?
        };

        Ok(ExportedAccounts {
            source_format: "cpr",
            accounts: accounts.into_iter().map(ExportedAccount::from).collect(),
        })
    }

    async fn export_all_accounts(&self) -> Result<Vec<StoredAccount>, AdminAccountError> {
        let mut accounts = Vec::new();
        let mut cursor = None;
        loop {
            let page = self
                .store
                .list(cursor, EXPORT_PAGE_LIMIT)
                .await
                .map_err(|_| AdminAccountError::Export)?;
            accounts.extend(page.items);
            let Some(next_cursor) = page.next_cursor else {
                return Ok(accounts);
            };
            cursor = Some(next_cursor);
        }
    }

    async fn export_accounts_by_id(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<StoredAccount>, AdminAccountError> {
        let mut accounts = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(account) = self
                .store
                .get(&id)
                .await
                .map_err(|_| AdminAccountError::Export)?
            else {
                return Err(AdminAccountError::NotFound);
            };
            accounts.push(account);
        }
        Ok(accounts)
    }
}

impl From<StoredAccount> for ExportedAccount {
    fn from(account: StoredAccount) -> Self {
        Self {
            id: account.id,
            email: account.email,
            account_id: account.account_id,
            user_id: account.user_id,
            label: account.label,
            plan_type: account.plan_type,
            token: account.access_token.expose_secret().to_string(),
            refresh_token: account
                .refresh_token
                .map(|token| token.expose_secret().to_string()),
            access_token_expires_at: account
                .access_token_expires_at
                .map(|value| value.to_rfc3339()),
            status: export_status_str(account.status).to_string(),
            added_at: china_rfc3339_str(&account.added_at),
            updated_at: china_rfc3339_str(&account.updated_at),
        }
    }
}

fn export_status_str(status: AccountStatus) -> &'static str {
    match status {
        AccountStatus::Active => "active",
        AccountStatus::Expired => "expired",
        AccountStatus::QuotaExhausted => "quota_exhausted",
        AccountStatus::Refreshing => "refreshing",
        AccountStatus::Disabled => "disabled",
        AccountStatus::Banned => "banned",
    }
}
