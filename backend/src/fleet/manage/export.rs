use secrecy::ExposeSecret;

use super::{
    AccountManageError, AccountManageService,
    types::{ExportedAccount, ExportedAccounts},
};
use crate::{fleet::store::StoredAccount, infra::time::china_rfc3339_str};

impl AccountManageService {
    pub async fn export(&self, ids: Vec<String>) -> Result<ExportedAccounts, AccountManageError> {
        if ids.is_empty() {
            return Err(AccountManageError::EmptyIds);
        }
        let accounts = self.export_accounts_by_id(ids).await?;

        Ok(ExportedAccounts {
            source_format: "cpr",
            accounts: accounts.into_iter().map(ExportedAccount::from).collect(),
        })
    }

    async fn export_accounts_by_id(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<StoredAccount>, AccountManageError> {
        let mut accounts = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(account) = self
                .store
                .get(&id)
                .await
                .map_err(|_| AccountManageError::Export)?
            else {
                return Err(AccountManageError::NotFound);
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
            status: account.status.as_str().to_string(),
            added_at: china_rfc3339_str(&account.added_at),
            updated_at: china_rfc3339_str(&account.updated_at),
        }
    }
}
