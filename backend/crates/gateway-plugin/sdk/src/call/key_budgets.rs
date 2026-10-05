//! Client Key 预算查询、上限更新与用量重置

use serde::{Deserialize, Serialize};

pub const GET: &str = "host.keys.get_budget";
pub const UPDATE_LIMITS: &str = "host.keys.update_budget_limits";
pub const RESET: &str = "host.keys.reset_budget";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetPeriod {
    Daily,
    Weekly,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetKeyBudgetRequest {
    pub client_key_id: String,
    pub period: BudgetPeriod,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetKeyBudgetResult {
    pub client_key_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetKeyBudgetRequest {
    pub client_key_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyBudget {
    pub client_key_id: String,
    pub daily_limit_usd: String,
    pub weekly_limit_usd: String,
    pub daily_used_usd: String,
    pub weekly_used_usd: String,
    pub daily_resets_at_ms: Option<i64>,
    pub weekly_resets_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateKeyBudgetLimitsRequest {
    pub client_key_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_limit_usd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekly_limit_usd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateKeyBudgetLimitsResult {
    pub client_key_id: String,
}
