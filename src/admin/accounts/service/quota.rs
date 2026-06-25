use chrono::Utc;
use secrecy::ExposeSecret;

use crate::infra::time::china_rfc3339;

use super::{types::AdminAccountError, AdminAccountService};

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
    pub async fn quota_warnings(&self) -> Result<serde_json::Value, AdminAccountError> {
        let snapshots = self
            .store
            .list_quota_snapshots()
            .await
            .map_err(|_| AdminAccountError::QuotaWarnings)?;

        let mut warnings = Vec::new();
        for snap in &snapshots {
            let quota: serde_json::Value =
                serde_json::from_str(&snap.quota_json).unwrap_or(serde_json::Value::Null);
            let used = crate::upstream::accounts::quota::quota_snapshot_limit_reached(&quota);
            if used {
                warnings.push(serde_json::json!({
                    "accountId": snap.account_id,
                    "email": snap.email,
                    "level": "exhausted"
                }));
            } else {
                for (quota_key, used_percent, role) in quota_warning_windows(&quota) {
                    let thresholds = match role {
                        "secondary" => &self.quota_thresholds.secondary,
                        _ => &self.quota_thresholds.primary,
                    };
                    for threshold in thresholds {
                        if used_percent >= f64::from(*threshold) {
                            warnings.push(serde_json::json!({
                                "accountId": snap.account_id,
                                "email": snap.email,
                                "level": "warning",
                                "threshold": threshold,
                                "usedPercent": used_percent,
                                "quotaKey": quota_key,
                            }));
                            break;
                        }
                    }
                }
            }
        }

        Ok(serde_json::json!({
            "warnings": warnings,
            "updatedAt": china_rfc3339(&Utc::now())
        }))
    }
    pub async fn health_check_accounts(
        &self,
        req: serde_json::Value,
    ) -> Result<serde_json::Value, AdminAccountError> {
        use crate::upstream::accounts::store::StoredAccount;

        let ids = req
            .get("ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut results = Vec::new();
        let accounts = if ids.is_empty() {
            let mut cursor = None;
            let mut all: Vec<StoredAccount> = Vec::new();
            loop {
                let page = self
                    .store
                    .list(cursor, 200)
                    .await
                    .map_err(|_| AdminAccountError::HealthCheck)?;
                all.extend(page.items);
                if page.next_cursor.is_none() {
                    break;
                }
                cursor = page.next_cursor;
            }
            all
        } else {
            let mut list = Vec::with_capacity(ids.len());
            for id in ids {
                if let Ok(Some(acct)) = self.store.get(&id).await {
                    list.push(acct);
                }
            }
            list
        };

        let request_id = uuid::Uuid::new_v4().to_string();
        for account in &accounts {
            let token = account.access_token.expose_secret().to_string();
            let cookie_header = self.usage_cookie_header(&account.id).await;
            let start = std::time::Instant::now();
            let context = crate::upstream::transport::CodexRequestContext {
                access_token: &token,
                account_id: account.account_id.as_deref(),
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
            match self.codex.fetch_usage(context).await {
                Ok(_) => {
                    let duration = start.elapsed().as_millis();
                    results.push(serde_json::json!({
                        "id": account.id,
                        "email": account.email,
                        "result": "alive",
                        "durationMs": duration
                    }));
                }
                Err(e) => {
                    let duration = start.elapsed().as_millis();
                    results.push(serde_json::json!({
                        "id": account.id,
                        "email": account.email,
                        "result": "dead",
                        "error": e.to_string(),
                        "durationMs": duration
                    }));
                }
            }
        }

        let total = results.len();
        let alive = results
            .iter()
            .filter(|r| r.get("result") == Some(&serde_json::json!("alive")))
            .count();
        let dead = total - alive;

        Ok(serde_json::json!({
            "summary": { "total": total, "alive": alive, "dead": dead, "skipped": 0 },
            "results": results
        }))
    }
}

fn quota_warning_windows(quota: &serde_json::Value) -> Vec<(String, f64, &'static str)> {
    let mut windows = Vec::new();
    if let Some(monthly) = quota.get("monthly_limit").filter(|value| !value.is_null()) {
        if let Some(used_percent) = used_percent(monthly) {
            windows.push(("monthly_limit".to_string(), used_percent, "primary"));
        }
    }
    if let Some(snapshots) = quota.get("snapshots").and_then(serde_json::Value::as_array) {
        for snapshot in snapshots {
            let source = snapshot
                .get("source")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("quota");
            let limit_name = snapshot
                .get("limit_name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(source);
            for role in ["primary", "secondary"] {
                let Some(window) = snapshot.get(role).filter(|value| !value.is_null()) else {
                    continue;
                };
                if let Some(used_percent) = used_percent(window) {
                    windows.push((format!("{source}:{limit_name}:{role}"), used_percent, role));
                }
            }
        }
    }
    windows
}

fn used_percent(value: &serde_json::Value) -> Option<f64> {
    value
        .get("used_percent")
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()))
        })
        .filter(|value| value.is_finite())
}
