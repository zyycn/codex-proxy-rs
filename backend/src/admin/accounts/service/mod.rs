//! 管理端账号业务服务。

mod contracts;
mod cookies;
mod exporting;
mod importing;
mod lifecycle;
pub(crate) mod oauth;
mod quota;
mod testing;

use std::sync::Arc as StdArc;

use chrono::{DateTime, Utc};

use crate::{
    upstream::accounts::{
        cookies::SqliteCookieStore,
        pool::RuntimeAccountPoolService,
        store::SqliteAccountStore,
        token_refresh::{RuntimeRefreshPolicy, TokenRefresher},
    },
    upstream::models::service::ModelService,
    upstream::transport::CodexBackendClient,
};

pub use contracts::{
    AdminAccountError, AdminAccountHealthCheck, AdminAccountMetadata, AdminAccountRefreshOutcome,
    AdminAccountRefreshResult, AdminAccountUpdate, BatchDeleteAccounts, ExportedAccounts,
    ImportedAccounts, OAuthAuthorizeResult, OAuthExchangeInput,
};

#[derive(Clone)]
pub struct AdminAccountService {
    pub store: SqliteAccountStore,
    pub(crate) cookies: SqliteCookieStore,
    pub(crate) codex: StdArc<CodexBackendClient>,
    pub(crate) models: StdArc<ModelService>,
    pub(crate) account_pool: StdArc<RuntimeAccountPoolService>,
    pub(crate) token_refresher: StdArc<dyn TokenRefresher>,
    pub(crate) oauth: oauth::AccountOAuthService,
    pub(crate) refresh_policy: RuntimeRefreshPolicy,
    pub(crate) installation_id: Option<String>,
}

pub struct AdminAccountServiceParts {
    pub store: SqliteAccountStore,
    pub cookies: SqliteCookieStore,
    pub codex: StdArc<CodexBackendClient>,
    pub models: StdArc<ModelService>,
    pub account_pool: StdArc<RuntimeAccountPoolService>,
    pub token_refresher: StdArc<dyn TokenRefresher>,
    pub oauth: oauth::AccountOAuthService,
    pub refresh_policy: RuntimeRefreshPolicy,
    pub installation_id: Option<String>,
}

impl AdminAccountService {
    pub fn new(parts: AdminAccountServiceParts) -> Self {
        Self {
            store: parts.store,
            cookies: parts.cookies,
            codex: parts.codex,
            models: parts.models,
            account_pool: parts.account_pool,
            token_refresher: parts.token_refresher,
            oauth: parts.oauth,
            refresh_policy: parts.refresh_policy,
            installation_id: parts.installation_id,
        }
    }

    pub(crate) fn next_refresh_at_for_expires_at(
        &self,
        account_id: &str,
        expires_at: DateTime<chrono::Utc>,
    ) -> DateTime<chrono::Utc> {
        crate::upstream::accounts::token_refresh::jittered_refresh_at(
            account_id,
            expires_at,
            self.refresh_policy.refresh_margin_seconds(),
        )
    }

    async fn refresh_tokens_from_refresh_token(
        &self,
        refresh_token: &str,
    ) -> Result<contracts::RefreshedAccountTokens, AdminAccountError> {
        let token_pair = self
            .token_refresher
            .refresh(refresh_token)
            .await
            .map_err(AdminAccountError::RefreshTokenExchange)?;
        let access_token = crate::upstream::accounts::importing::normalize_nonempty(Some(
            crate::upstream::accounts::importing::normalize_bearer_token(&token_pair.access_token),
        ))
        .ok_or(AdminAccountError::TokenRequired)?;
        let claims = crate::upstream::accounts::token_refresh::manual_account_claims(
            &access_token,
            Utc::now(),
        )
        .map_err(AdminAccountError::InvalidToken)?;

        Ok(contracts::RefreshedAccountTokens {
            access_token,
            refresh_token: token_pair.refresh_token,
            claims,
        })
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
