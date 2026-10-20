//! API Key 上游余额与套餐额度的只读展示合同

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountBalance {
    pub query_type: String,
    pub observed_at: Option<DateTime<Utc>>,
    pub entries: Vec<AccountBalanceEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountBalanceEntry {
    pub label: String,
    pub remaining: Option<String>,
    pub used: Option<String>,
    pub total: Option<String>,
    pub unit: String,
    pub unlimited: bool,
    pub is_valid: bool,
    pub invalid_message: Option<String>,
    pub window_seconds: Option<u64>,
    pub reset_at: Option<DateTime<Utc>>,
    pub details: Vec<AccountBalanceDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountBalanceDetail {
    pub label: String,
    pub amount: String,
}
