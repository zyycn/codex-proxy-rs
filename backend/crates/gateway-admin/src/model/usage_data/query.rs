//! 请求明细的有界筛选与稳定顺序分页

use super::UsageRecord;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetUsageRecord {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageRecordFilter {
    pub start_ms: i64,
    pub end_ms: i64,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub account_ref: Option<String>,
    pub client_key_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListUsageRecords {
    pub filter: UsageRecordFilter,
    pub cursor: Option<String>,
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageRecordPage {
    pub items: Vec<UsageRecord>,
    pub next_cursor: Option<String>,
}
