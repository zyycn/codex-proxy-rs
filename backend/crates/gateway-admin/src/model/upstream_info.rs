//! 管理端互联快照的中立展示事实，不携带 Provider 原始协议

use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUpstreamInfo {
    /// 当前连接曾成功识别互联，失权时保留身份但清除观测内容
    pub discovered: bool,
    pub observed_at: Option<DateTime<Utc>>,
    pub stale: bool,
    pub connection_revision: String,
    pub last_error: Option<String>,
    pub max_concurrency: Option<u64>,
    pub requests_per_minute: Option<u64>,
    pub pool: Option<UpstreamPool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamPool {
    pub account_count: u32,
    pub states: UpstreamPoolStates,
    pub plans: Vec<UpstreamPlanCount>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamPoolStates {
    pub ready: u32,
    pub cooldown: u32,
    pub exhausted: u32,
    pub disabled: u32,
    pub error: u32,
    pub unknown: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamPlanCount {
    pub name: String,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpstreamQuotaWindow {
    pub account_id: Option<String>,
    pub observed_at: Option<DateTime<Utc>>,
    pub stale: bool,
    pub amount: Option<UpstreamQuotaAmount>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamQuotaAmount {
    pub unit: String,
    pub used: String,
    pub remaining: Option<String>,
    pub total: Option<String>,
}
