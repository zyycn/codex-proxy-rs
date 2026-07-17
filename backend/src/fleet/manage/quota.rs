use serde_json::Value;

use super::{AccountManageService, types::AccountManageError};
use crate::fleet::{
    account_failure::{apply_account_state_effect, classify_account_failure},
    account_gateway::AccountUpstreamContext,
    quota::QuotaSnapshot,
};

pub struct AccountQuotaRefresh {
    pub quota: QuotaSnapshot,
    pub raw: Value,
}

impl AccountManageService {
    async fn usage_cookie_header(&self, account_id: &str) -> Option<String> {
        self.cookies
            .cookie_header_for_request(account_id, "chatgpt.com", "/codex/usage")
            .await
            .ok()
            .flatten()
    }

    pub async fn quota_snapshots(
        &self,
    ) -> Result<Vec<crate::fleet::store::AccountQuotaSnapshot>, AccountManageError> {
        self.store
            .list_quota_snapshots()
            .await
            .map_err(|_| AccountManageError::Inspect)
    }

    pub async fn account_quota(
        &self,
        account_id: &str,
    ) -> Result<AccountQuotaRefresh, AccountManageError> {
        let stored = self
            .store
            .get(account_id)
            .await
            .map_err(|_| AccountManageError::NotFound)?
            .ok_or(AccountManageError::NotFound)?;

        let context = AccountUpstreamContext {
            access_token: stored.access_token.clone(),
            account_id: stored.account_id.clone(),
            request_id: uuid::Uuid::new_v4().to_string(),
            cookie_header: self.usage_cookie_header(account_id).await,
            installation_id: Some(self.account_pseudonymizer.installation_id(account_id)),
        };
        let result = match self.upstream.fetch_usage(context).await {
            Ok(result) => result,
            Err(error) => {
                if let Some(classified) = error.failure().and_then(classify_account_failure)
                    && let Some(effect) = &classified.effect
                {
                    apply_account_state_effect(
                        &self.account_pool,
                        self.upstream.as_ref(),
                        account_id,
                        effect,
                    )
                    .await;
                }
                return Err(AccountManageError::FetchQuota(error.to_string()));
            }
        };
        self.account_pool
            .apply_quota_snapshot(account_id, &result.quota)
            .await;
        self.sync_account_pool_best_effort(account_id, "account quota refresh")
            .await;
        Ok(AccountQuotaRefresh {
            quota: result.quota,
            raw: result.raw,
        })
    }
}
