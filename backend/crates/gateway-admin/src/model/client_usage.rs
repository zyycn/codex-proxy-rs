//! Client API Key 自助用量查询事实。

use chrono::{DateTime, Utc};
use gateway_core::{
    engine::budget::ClientBudgetStatus,
    policy::{ClientApiKeyId, RateLimits},
};

use super::observability::{
    DashboardWireProfile, HealthTimeline, RequestMetricPoint, TimeRange, UsageListRecord,
    UsageOverview, UsageTotals,
};

/// 当前 Client 会话可见的安全 Key 摘要与账本状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientUsageKey {
    pub id: ClientApiKeyId,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub limits: RateLimits,
    pub budget: ClientBudgetStatus,
    pub last_used_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
}

/// 当前会话 Key 的概览事实。
#[derive(Debug, Clone, PartialEq)]
pub struct ClientOverview {
    pub as_of: DateTime<Utc>,
    pub key: ClientUsageKey,
    pub range: TimeRange,
    pub overview: UsageOverview,
    pub totals: UsageTotals,
    pub trend: Vec<RequestMetricPoint>,
    pub health_timeline: HealthTimeline,
    pub wire_profiles: Vec<DashboardWireProfile>,
    pub usage_records: Vec<UsageListRecord>,
}
