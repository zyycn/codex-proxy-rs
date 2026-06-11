use std::collections::BTreeMap;

use crate::accounts::model::{Account, AccountStatus};

#[derive(Debug, Default)]
pub struct AccountPool {
    accounts: BTreeMap<String, Account>,
}

impl AccountPool {
    pub fn insert(&mut self, account: Account) {
        self.accounts.insert(account.id.clone(), account);
    }

    pub fn acquire(&self, _model: &str) -> Option<Account> {
        self.accounts
            .values()
            .filter(|account| account.status == AccountStatus::Active)
            .min_by_key(|account| account.last_used_at.clone())
            .cloned()
    }
}
