//! 只读基础事实；版本 1 不提供凭据、预测、聚合或主动上游刷新。

use serde::{Deserialize, Serialize};

pub const ACCOUNTS_LIST: &str = "host.data.accounts.list";
pub const QUOTA_GET: &str = "host.data.quota.get";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountFactsQuery {
    pub provider_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountFacts {
    pub account_id: String,
    pub provider_id: String,
    pub group_ids: Vec<String>,
    pub enabled: bool,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountFactsPage {
    pub schema_version: u32,
    pub accounts: Vec<AccountFacts>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaFactsQuery {
    pub account_id: String,
}

/// Provider 已有快照的必要投影；空观测时间表示没有可用样本，不代表额度为零。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaFacts {
    pub schema_version: u32,
    pub account_id: String,
    pub observed_at_ms: Option<i64>,
    pub windows: Vec<QuotaWindowFacts>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaWindowFacts {
    pub key: String,
    pub window_seconds: Option<u64>,
    /// 百分比而非 0～1 比率；未知值为 null。
    pub used_percent: Option<f64>,
    pub reset_at_ms: Option<i64>,
}
