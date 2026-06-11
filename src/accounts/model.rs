use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AccountStatus {
    Active,
    Expired,
    QuotaExhausted,
    Refreshing,
    Disabled,
    Banned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub status: AccountStatus,
    pub added_at: String,
    pub last_used_at: Option<String>,
}

impl Account {
    pub fn test(id: &str, status: AccountStatus) -> Self {
        Self {
            id: id.to_string(),
            email: None,
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: format!("token-{id}"),
            refresh_token: Some(format!("refresh-{id}")),
            access_token_expires_at: Some(Utc::now() + Duration::hours(1)),
            status,
            added_at: Utc::now().to_rfc3339(),
            last_used_at: None,
        }
    }
}
