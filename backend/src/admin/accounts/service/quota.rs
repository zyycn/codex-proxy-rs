use secrecy::ExposeSecret;

use super::{contracts::AdminAccountError, AdminAccountService};

impl AdminAccountService {
    pub async fn account_quota(
        &self,
        account_id: &str,
    ) -> Result<serde_json::Value, AdminAccountError> {
        let stored = self
            .store
            .get(account_id)
            .await
            .map_err(|_| AdminAccountError::NotFound)?
            .ok_or(AdminAccountError::NotFound)?;

        let request_id = uuid::Uuid::new_v4().to_string();
        let token = stored.access_token.expose_secret().to_string();
        let cookie_header = self.usage_cookie_header(account_id).await;
        let context = crate::upstream::transport::CodexRequestContext {
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
            .map_err(|e| AdminAccountError::FetchQuota(e.to_string()))?;
        let normalized = crate::upstream::accounts::quota::quota_from_usage(&raw);
        if let Ok(json_str) = serde_json::to_string(&normalized) {
            if matches!(
                self.store.update_quota_json(account_id, &json_str).await,
                Ok(true)
            ) {
                self.sync_account_pool_best_effort(account_id, "account quota refresh")
                    .await;
            }
        }
        Ok(serde_json::json!({ "quota": normalized, "raw": raw }))
    }
}
