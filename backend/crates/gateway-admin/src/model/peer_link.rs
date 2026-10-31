//! 当前认证 Key 可公开的本地互联观测，不携带 HTTP 或 Provider 私有字段

use super::provider_credentials::ProviderQuotaWindowRole;
use chrono::{DateTime, Utc};
use gateway_core::engine::budget::ClientBudgetStatus;

/// 调用边界提供的响应资源预算，超限时整体拒绝而不截断授权范围
#[derive(Debug, Clone, Copy)]
pub struct PeerLinkLimits {
    pub max_accounts: usize,
    pub max_quota_windows: usize,
    pub max_models: usize,
    pub max_response_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerNodeIdentity {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PeerLinkExport {
    pub node: PeerNodeIdentity,
    pub observed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub budget: ClientBudgetStatus,
    pub budget_observed_at: DateTime<Utc>,
    pub pool: Option<PeerPoolSummary>,
    pub quota: Option<Vec<PeerAccountQuota>>,
    pub models: Vec<PeerModelSummary>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerPoolSummary {
    pub ready: u32,
    pub cooldown: u32,
    pub exhausted: u32,
    pub disabled: u32,
    pub error: u32,
    pub unknown: u32,
    pub plans: Option<std::collections::BTreeMap<String, u32>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PeerAccountQuota {
    pub id: String,
    pub models: Vec<String>,
    pub observed_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub windows: Vec<PeerQuotaWindow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PeerQuotaWindow {
    pub key: String,
    pub group: String,
    pub label: String,
    pub limit_id: Option<String>,
    pub limit_name: Option<String>,
    pub role: Option<ProviderQuotaWindowRole>,
    pub window_seconds: Option<u64>,
    pub used_percent: Option<f64>,
    pub reset_at: Option<DateTime<Utc>>,
    pub limit_reached: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerModelState {
    Available,
    Exhausted,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerModelSummary {
    pub model: String,
    pub complete: bool,
    pub state: PeerModelState,
    pub observed_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}
