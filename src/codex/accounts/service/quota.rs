use crate::codex::accounts::model::AccountStatus;

use super::{
    health::{apply_codex_account_error, fetch_account_usage, public_codex_error},
    AccountQuotaError, AccountQuotaResult, AccountQuotaWarning, AccountQuotaWarnings,
    AccountService, AccountServiceError, QuotaWarningLevel, QuotaWarningWindow,
};

use serde_json::{json, Map, Value};

impl AccountService {
    pub async fn account_quota(
        &self,
        account_id: &str,
        request_id: &str,
    ) -> Result<AccountQuotaResult, AccountQuotaError> {
        let repo = self
            .repository
            .as_ref()
            .ok_or(AccountQuotaError::RepositoryUnavailable)?;
        let account = match repo.get(account_id).await {
            Ok(Some(account)) => account,
            Ok(None) => return Err(AccountQuotaError::NotFound),
            Err(_) => return Err(AccountQuotaError::Load),
        };
        if account.status != AccountStatus::Active {
            return Err(AccountQuotaError::Inactive(account.status));
        }

        match fetch_account_usage(self, &account, request_id).await {
            Ok(raw) => {
                let quota = quota_from_usage(&raw);
                if repo
                    .update_quota_json(&account.id, &quota.to_string())
                    .await
                    .is_err()
                {
                    return Err(AccountQuotaError::StoreQuota);
                }
                Ok(AccountQuotaResult { quota, raw })
            }
            Err(error) => {
                apply_codex_account_error(self, repo, &account, &error).await;
                Err(AccountQuotaError::Fetch(public_codex_error(&error)))
            }
        }
    }

    pub async fn quota_warnings(&self) -> Result<AccountQuotaWarnings, AccountServiceError> {
        let snapshots = self
            .repository()?
            .list_quota_snapshots()
            .await
            .map_err(|_| AccountServiceError::QuotaWarnings)?;
        let primary_thresholds = sorted_thresholds(&self.config.quota.warning_thresholds.primary);
        let secondary_thresholds =
            sorted_thresholds(&self.config.quota.warning_thresholds.secondary);
        let mut warnings = Vec::new();
        let mut updated_at = None;

        for snapshot in snapshots {
            let Ok(quota) = serde_json::from_str::<Value>(&snapshot.quota_json) else {
                continue;
            };
            let before_len = warnings.len();
            if let Some(warning) = warning_from_quota_window(
                &snapshot.account_id,
                snapshot.email.as_deref(),
                &quota,
                "rate_limit",
                QuotaWarningWindow::Primary,
                &primary_thresholds,
            ) {
                warnings.push(warning);
            }
            if let Some(warning) = warning_from_quota_window(
                &snapshot.account_id,
                snapshot.email.as_deref(),
                &quota,
                "secondary_rate_limit",
                QuotaWarningWindow::Secondary,
                &secondary_thresholds,
            ) {
                warnings.push(warning);
            }
            if warnings.len() > before_len {
                updated_at = max_optional_datetime(updated_at, snapshot.quota_fetched_at);
            }
        }

        Ok(AccountQuotaWarnings {
            warnings,
            updated_at,
        })
    }
}

fn warning_from_quota_window(
    account_id: &str,
    email: Option<&str>,
    quota: &Value,
    field: &str,
    window: QuotaWarningWindow,
    thresholds: &[u8],
) -> Option<AccountQuotaWarning> {
    let quota_window = quota.get(field).filter(|value| !value.is_null())?;
    let used_percent = quota_window
        .get("used_percent")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())?;
    let level = warning_level(used_percent, thresholds)?;

    Some(AccountQuotaWarning {
        account_id: account_id.to_string(),
        email: email.map(str::to_string),
        window,
        level,
        used_percent,
        reset_at: quota_window.get("reset_at").and_then(Value::as_i64),
    })
}

fn warning_level(used_percent: f64, thresholds: &[u8]) -> Option<QuotaWarningLevel> {
    let matched_index = thresholds
        .iter()
        .rposition(|threshold| used_percent >= f64::from(*threshold))?;
    if matched_index + 1 == thresholds.len() {
        Some(QuotaWarningLevel::Critical)
    } else {
        Some(QuotaWarningLevel::Warning)
    }
}

fn sorted_thresholds(thresholds: &[u8]) -> Vec<u8> {
    let mut thresholds = thresholds.to_vec();
    thresholds.sort_unstable();
    thresholds.dedup();
    thresholds
}

fn max_optional_datetime(
    current: Option<chrono::DateTime<chrono::Utc>>,
    candidate: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.max(candidate)),
        (Some(current), None) => Some(current),
        (None, Some(candidate)) => Some(candidate),
        (None, None) => None,
    }
}

pub(super) fn quota_from_usage(usage: &Value) -> Value {
    let additional = usage
        .get("additional_rate_limits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut rate_limits_by_limit_id = Map::new();
    for item in &additional {
        let Some(limit_id) = item
            .get("metered_feature")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let quota = quota_from_rate_limit(item.get("rate_limit"));
        if quota.is_null() {
            continue;
        }
        rate_limits_by_limit_id.insert(
            limit_id.to_string(),
            json!({
                "limit_id": limit_id,
                "limit_name": item.get("limit_name").cloned().unwrap_or(Value::Null),
                "allowed": quota.get("allowed").cloned().unwrap_or(Value::Null),
                "limit_reached": quota.get("limit_reached").cloned().unwrap_or(Value::Null),
                "used_percent": quota.get("used_percent").cloned().unwrap_or(Value::Null),
                "remaining_percent": quota.get("remaining_percent").cloned().unwrap_or(Value::Null),
                "reset_at": quota.get("reset_at").cloned().unwrap_or(Value::Null),
                "limit_window_seconds": quota.get("limit_window_seconds").cloned().unwrap_or(Value::Null),
                "secondary_rate_limit": secondary_quota_from_rate_limit(item.get("rate_limit")),
            }),
        );
    }
    let additional_review = additional.iter().find(|item| {
        is_review_limit_id(item.get("metered_feature").and_then(Value::as_str))
            || is_review_limit_id(item.get("limit_name").and_then(Value::as_str))
    });
    let code_review_rate_limit = match quota_from_rate_limit(usage.get("code_review_rate_limit")) {
        Value::Null => {
            quota_from_rate_limit(additional_review.and_then(|item| item.get("rate_limit")))
        }
        quota => quota,
    };

    json!({
        "plan_type": usage.get("plan_type").cloned().unwrap_or(Value::Null),
        "rate_limit": quota_from_rate_limit(usage.get("rate_limit")),
        "secondary_rate_limit": secondary_quota_from_rate_limit(usage.get("rate_limit")),
        "code_review_rate_limit": code_review_rate_limit,
        "rate_limits_by_limit_id": if rate_limits_by_limit_id.is_empty() {
            Value::Null
        } else {
            Value::Object(rate_limits_by_limit_id)
        },
        "credits": normalize_quota_credits(usage.get("credits")),
    })
}

fn quota_from_rate_limit(rate_limit: Option<&Value>) -> Value {
    let Some(rate_limit) = rate_limit.filter(|value| !value.is_null()) else {
        return Value::Null;
    };
    let primary = rate_limit.get("primary_window");
    let used_percent = primary
        .and_then(|window| window.get("used_percent"))
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "allowed": rate_limit.get("allowed").cloned().unwrap_or(Value::Null),
        "limit_reached": rate_limit.get("limit_reached").cloned().unwrap_or(Value::Null),
        "used_percent": used_percent,
        "remaining_percent": remaining_percent(primary.and_then(|window| window.get("used_percent"))),
        "reset_at": primary.and_then(|window| window.get("reset_at")).cloned().unwrap_or(Value::Null),
        "limit_window_seconds": primary.and_then(|window| window.get("limit_window_seconds")).cloned().unwrap_or(Value::Null),
    })
}

fn secondary_quota_from_rate_limit(rate_limit: Option<&Value>) -> Value {
    let Some(secondary) = rate_limit
        .and_then(|rate_limit| rate_limit.get("secondary_window"))
        .filter(|value| !value.is_null())
    else {
        return Value::Null;
    };
    let used_percent = secondary
        .get("used_percent")
        .cloned()
        .unwrap_or(Value::Null);
    let limit_reached = secondary
        .get("used_percent")
        .and_then(Value::as_f64)
        .map(|used| used >= 100.0)
        .map(Value::Bool)
        .or_else(|| {
            rate_limit
                .and_then(|rate_limit| rate_limit.get("limit_reached"))
                .cloned()
        })
        .unwrap_or(Value::Null);
    json!({
        "limit_reached": limit_reached,
        "used_percent": used_percent,
        "remaining_percent": remaining_percent(secondary.get("used_percent")),
        "reset_at": secondary.get("reset_at").cloned().unwrap_or(Value::Null),
        "limit_window_seconds": secondary.get("limit_window_seconds").cloned().unwrap_or(Value::Null),
    })
}

fn normalize_quota_credits(raw: Option<&Value>) -> Value {
    let Some(raw) = raw.filter(|value| !value.is_null()) else {
        return Value::Null;
    };
    let Some(balance) = raw
        .get("balance")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
    else {
        return Value::Null;
    };
    json!({
        "has_credits": raw.get("has_credits").and_then(Value::as_bool).unwrap_or(false),
        "unlimited": raw.get("unlimited").and_then(Value::as_bool).unwrap_or(false),
        "overage_limit_reached": raw.get("overage_limit_reached").and_then(Value::as_bool).unwrap_or(false),
        "balance": balance,
    })
}

fn remaining_percent(used_percent: Option<&Value>) -> Value {
    let Some(used_percent) = used_percent.and_then(Value::as_f64) else {
        return Value::Null;
    };
    json!((100.0 - used_percent.clamp(0.0, 100.0)).round() as i64)
}

fn is_review_limit_id(value: Option<&str>) -> bool {
    let normalized = value
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_");
    normalized == "review"
        || normalized == "code_review"
        || normalized == "codex_review"
        || normalized == "codex_code_review"
        || normalized.contains("code_review")
        || normalized.contains("codex_review")
}
