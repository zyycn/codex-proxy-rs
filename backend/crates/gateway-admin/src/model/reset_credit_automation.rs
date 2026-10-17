//! 自动重置卡的持久计划与查询投影，不保存上游凭据或完整卡片

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedResetCredit {
    pub id: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingResetCredit {
    pub credit: PlannedResetCredit,
    pub redeem_request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetCreditAutomationState {
    pub next_discovery_at: DateTime<Utc>,
    pub credits: Vec<PlannedResetCredit>,
    pub pending: Option<PendingResetCredit>,
    pub retry_at: Option<DateTime<Utc>>,
    pub failures: u32,
    pub last_result: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetCreditAutomation {
    pub account_id: String,
    pub enabled: bool,
    /// 零表示尚未创建；每次持久状态写入都递增版本
    pub revision: u64,
    pub next_run_at: DateTime<Utc>,
    pub state: ResetCreditAutomationState,
}

#[derive(Debug, Clone)]
pub struct ResetCreditAutomationStatus {
    pub enabled: bool,
    pub lead_minutes: u32,
    pub next_discovery_at: Option<DateTime<Utc>>,
    pub next_consume_at: Option<DateTime<Utc>>,
    pub last_result: Option<String>,
}
