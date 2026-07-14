//! 账号运行状态的有序后台持久化。

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;

use crate::fleet::{account::AccountStatus, store::AccountStore};

#[derive(Clone)]
pub(super) struct AccountStatePersistence {
    sender: mpsc::UnboundedSender<AccountStateWrite>,
}

enum AccountStateWrite {
    SetStatus {
        account_id: String,
        status: AccountStatus,
    },
    MarkQuotaLimitedUntil {
        account_id: String,
        until: DateTime<Utc>,
    },
}

impl AccountStatePersistence {
    pub(super) fn new(store: Arc<dyn AccountStore>) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        tokio::spawn(run(receiver, store));
        Self { sender }
    }

    pub(super) fn set_status(&self, account_id: &str, status: AccountStatus) {
        self.enqueue(AccountStateWrite::SetStatus {
            account_id: account_id.to_owned(),
            status,
        });
    }

    pub(super) fn mark_quota_limited_until(&self, account_id: &str, until: DateTime<Utc>) {
        self.enqueue(AccountStateWrite::MarkQuotaLimitedUntil {
            account_id: account_id.to_owned(),
            until,
        });
    }

    fn enqueue(&self, write: AccountStateWrite) {
        if self.sender.send(write).is_err() {
            tracing::error!("Account state persistence worker stopped unexpectedly");
        }
    }
}

async fn run(
    mut receiver: mpsc::UnboundedReceiver<AccountStateWrite>,
    store: Arc<dyn AccountStore>,
) {
    while let Some(write) = receiver.recv().await {
        match write {
            AccountStateWrite::SetStatus { account_id, status } => {
                match store.set_status(&account_id, status).await {
                    Ok(true) => {}
                    Ok(false) => tracing::warn!(
                        account_id,
                        ?status,
                        "Account disappeared before deferred status persistence"
                    ),
                    Err(error) => tracing::error!(
                        account_id,
                        ?status,
                        error = %error,
                        "Failed to persist deferred account status"
                    ),
                }
            }
            AccountStateWrite::MarkQuotaLimitedUntil { account_id, until } => {
                match store.mark_quota_limited_until(&account_id, until).await {
                    Ok(true) => {}
                    Ok(false) => tracing::warn!(
                        account_id,
                        %until,
                        "Account disappeared before deferred quota persistence"
                    ),
                    Err(error) => tracing::error!(
                        account_id,
                        %until,
                        error = %error,
                        "Failed to persist deferred quota state"
                    ),
                }
            }
        }
    }
}
