use secrecy::ExposeSecret;

use super::{types::AccountManageError, AccountManageService};

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
    ) -> Result<serde_json::Value, AccountManageError> {
        let stored = self
            .store
            .get(account_id)
            .await
            .map_err(|_| AccountManageError::NotFound)?
            .ok_or(AccountManageError::NotFound)?;

        let request_id = uuid::Uuid::new_v4().to_string();
        let token = stored.access_token.expose_secret().to_string();
        let cookie_header = self.usage_cookie_header(account_id).await;
        let context = crate::upstream::openai::transport::CodexRequestContext {
            access_token: &token,
            account_id: stored.account_id.as_deref(),
            request_id: &request_id,
            turn_state: None,
            turn_metadata: None,
            beta_features: None,
            include_timing_metrics: None,
            version: None,
            codex_window_id: None,
            parent_thread_id: None,
            cookie_header: cookie_header.as_deref(),
            installation_id: self.installation_id.as_deref(),
            session_id: None,
        };
        let raw = self
            .codex
            .fetch_usage(context)
            .await
            .map_err(|e| AccountManageError::FetchQuota(e.to_string()))?;
        let normalized = crate::fleet::quota::quota_from_usage(&raw);
        self.account_pool
            .apply_quota_snapshot(account_id, &normalized)
            .await;
        self.sync_account_pool_best_effort(account_id, "account quota refresh")
            .await;
        Ok(serde_json::json!({ "quota": normalized, "raw": raw }))
    }
}
