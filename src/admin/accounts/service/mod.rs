//! 管理端账号业务服务。

mod cookies;
mod importing;
mod lifecycle;
mod quota;
mod types;

use std::sync::Arc as StdArc;

use chrono::{DateTime, Duration};

use crate::{
    config::types::QuotaWarningThresholds,
    upstream::accounts::{
        cookies::SqliteCookieStore, pool::RuntimeAccountPoolService, store::SqliteAccountStore,
        token_refresh::TokenRefresher,
    },
    upstream::transport::CodexBackendClient,
};

pub use types::{
    AdminAccountError, AdminAccountMetadata, AdminAccountMetadataUpdate, BatchDeleteAccounts,
    BatchUpdateAccountStatus, ImportedAccounts, UpdatedAccountStatus,
};

#[derive(Clone)]
pub struct AdminAccountService {
    pub store: SqliteAccountStore,
    pub(crate) cookies: SqliteCookieStore,
    pub(crate) quota_thresholds: QuotaWarningThresholds,
    pub(crate) codex: StdArc<CodexBackendClient>,
    pub(crate) account_pool: StdArc<RuntimeAccountPoolService>,
    pub(crate) token_refresher: StdArc<dyn TokenRefresher>,
    pub(crate) refresh_margin_seconds: u64,
    pub(crate) installation_id: Option<String>,
}

impl AdminAccountService {
    #[expect(
        clippy::too_many_arguments,
        reason = "service constructor wires independent stores and runtime collaborators"
    )]
    pub fn new(
        store: SqliteAccountStore,
        cookies: SqliteCookieStore,
        quota_thresholds: QuotaWarningThresholds,
        codex: StdArc<CodexBackendClient>,
        account_pool: StdArc<RuntimeAccountPoolService>,
        token_refresher: StdArc<dyn TokenRefresher>,
        refresh_margin_seconds: u64,
        installation_id: Option<String>,
    ) -> Self {
        Self {
            store,
            cookies,
            quota_thresholds,
            codex,
            account_pool,
            token_refresher,
            refresh_margin_seconds,
            installation_id,
        }
    }

    pub(crate) fn next_refresh_at_for_expires_at(
        &self,
        expires_at: DateTime<chrono::Utc>,
    ) -> DateTime<chrono::Utc> {
        let margin_seconds = self.refresh_margin_seconds.min(i64::MAX as u64) as i64;
        expires_at - Duration::seconds(margin_seconds)
    }

    pub(crate) async fn sync_account_pool(
        &self,
        account_id: &str,
    ) -> Result<(), AdminAccountError> {
        self.account_pool
            .sync_account_from_repository(account_id)
            .await
            .map(|_| ())
            .map_err(|_| AdminAccountError::SyncAccountPool)
    }

    pub(crate) async fn sync_account_pool_best_effort(&self, account_id: &str, operation: &str) {
        if let Err(error) = self
            .account_pool
            .sync_account_from_repository(account_id)
            .await
        {
            tracing::warn!(
                account_id,
                operation,
                error = %error,
                "failed to sync runtime account pool after admin account update"
            );
        }
    }

    pub(crate) async fn evict_account_websocket_pool(&self, account_id: &str) {
        self.codex.evict_websocket_account(account_id).await;
        match self.store.get(account_id).await {
            Ok(Some(account)) => {
                if let Some(upstream_account_id) = account
                    .account_id
                    .as_deref()
                    .filter(|value| *value != account_id)
                {
                    self.codex
                        .evict_websocket_account(upstream_account_id)
                        .await;
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to inspect account while evicting websocket pool"
                );
            }
        }
    }
}
