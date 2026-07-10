//! 管理端账号业务服务。

use std::sync::Arc;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::oauth;

use crate::{
    accounts::{
        cookies::PgCookieStore,
        pool::RuntimeAccountPoolService,
        refresh::{RedisRefreshLeaseStore, RuntimeRefreshPolicy},
        store::PgAccountStore,
    },
    models::service::ModelService,
    upstream::openai::{token_client::TokenRefresher, transport::CodexBackendClient},
};

use super::types::{AccountManageError, RefreshedAccountTokens};

#[derive(Clone)]
pub struct AccountManageService {
    pub store: PgAccountStore,
    pub(crate) cookies: PgCookieStore,
    pub(crate) codex: Arc<CodexBackendClient>,
    pub(crate) models: Arc<ModelService>,
    pub(crate) account_pool: Arc<RuntimeAccountPoolService>,
    pub(crate) token_refresher: Arc<dyn TokenRefresher>,
    pub(crate) refresh_leases: RedisRefreshLeaseStore,
    pub(crate) refresh_lease_owner_prefix: String,
    pub(crate) oauth: oauth::AccountOAuthService,
    pub(crate) refresh_policy: RuntimeRefreshPolicy,
    pub(crate) installation_id: Option<String>,
}

pub struct AccountManageServiceParts {
    pub store: PgAccountStore,
    pub cookies: PgCookieStore,
    pub codex: Arc<CodexBackendClient>,
    pub models: Arc<ModelService>,
    pub account_pool: Arc<RuntimeAccountPoolService>,
    pub token_refresher: Arc<dyn TokenRefresher>,
    pub refresh_leases: RedisRefreshLeaseStore,
    pub oauth: oauth::AccountOAuthService,
    pub refresh_policy: RuntimeRefreshPolicy,
    pub installation_id: Option<String>,
}

impl AccountManageService {
    pub fn new(parts: AccountManageServiceParts) -> Self {
        Self {
            store: parts.store,
            cookies: parts.cookies,
            codex: parts.codex,
            models: parts.models,
            account_pool: parts.account_pool,
            token_refresher: parts.token_refresher,
            refresh_leases: parts.refresh_leases,
            refresh_lease_owner_prefix: format!(
                "admin-account-refresh:{}",
                Uuid::new_v4().simple()
            ),
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
        crate::accounts::refresh::jittered_refresh_at(
            account_id,
            expires_at,
            self.refresh_policy.refresh_margin_seconds(),
        )
    }

    pub(super) async fn refresh_tokens_from_refresh_token(
        &self,
        refresh_token: &str,
    ) -> Result<RefreshedAccountTokens, AccountManageError> {
        let token_pair = self
            .token_refresher
            .refresh(refresh_token)
            .await
            .map_err(AccountManageError::RefreshTokenExchange)?;
        let access_token = crate::accounts::import::normalize_nonempty(Some(
            crate::accounts::import::normalize_bearer_token(&token_pair.access_token),
        ))
        .ok_or(AccountManageError::TokenRequired)?;
        let claims = crate::accounts::refresh::manual_account_claims(&access_token, Utc::now())
            .map_err(AccountManageError::InvalidToken)?;

        Ok(RefreshedAccountTokens {
            access_token,
            refresh_token: token_pair.refresh_token,
            claims,
        })
    }

    pub(crate) async fn sync_account_pool(
        &self,
        account_id: &str,
    ) -> Result<(), AccountManageError> {
        self.account_pool
            .sync_account_from_store(account_id)
            .await
            .map(|_| ())
            .map_err(|_| AccountManageError::SyncAccountPool)
    }

    pub(crate) async fn sync_account_pool_best_effort(&self, account_id: &str, operation: &str) {
        if let Err(error) = self.account_pool.sync_account_from_store(account_id).await {
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
