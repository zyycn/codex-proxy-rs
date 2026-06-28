//! 管理端账号业务服务。

mod cookies;
mod exporting;
mod importing;
mod lifecycle;
pub(crate) mod oauth;
mod quota;
mod testing;
mod types;

use std::sync::Arc as StdArc;

use chrono::{DateTime, Duration};

use crate::{
    upstream::accounts::{
        cookies::SqliteCookieStore, pool::RuntimeAccountPoolService, store::SqliteAccountStore,
        token_refresh::TokenRefresher,
    },
    upstream::transport::CodexBackendClient,
};

pub use types::{
    AdminAccountError, AdminAccountMetadata, AdminAccountUpdate, BatchDeleteAccounts,
    ExportedAccounts, ImportedAccounts, OAuthAuthorizeResult, OAuthExchangeInput,
    UpdatedAccountStatus,
};

#[derive(Clone)]
pub struct AdminAccountService {
    pub store: SqliteAccountStore,
    pub(crate) cookies: SqliteCookieStore,
    pub(crate) codex: StdArc<CodexBackendClient>,
    pub(crate) account_pool: StdArc<RuntimeAccountPoolService>,
    pub(crate) token_refresher: StdArc<dyn TokenRefresher>,
    pub(crate) oauth: oauth::AccountOAuthService,
    pub(crate) refresh_margin_seconds: u64,
    pub(crate) installation_id: Option<String>,
}

pub struct AdminAccountServiceParts {
    pub store: SqliteAccountStore,
    pub cookies: SqliteCookieStore,
    pub codex: StdArc<CodexBackendClient>,
    pub account_pool: StdArc<RuntimeAccountPoolService>,
    pub token_refresher: StdArc<dyn TokenRefresher>,
    pub oauth: oauth::AccountOAuthService,
    pub refresh_margin_seconds: u64,
    pub installation_id: Option<String>,
}

impl AdminAccountService {
    pub fn new(parts: AdminAccountServiceParts) -> Self {
        Self {
            store: parts.store,
            cookies: parts.cookies,
            codex: parts.codex,
            account_pool: parts.account_pool,
            token_refresher: parts.token_refresher,
            oauth: parts.oauth,
            refresh_margin_seconds: parts.refresh_margin_seconds,
            installation_id: parts.installation_id,
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
