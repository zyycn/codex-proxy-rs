//! 管理端互联快照的中立展示事实，不携带 Provider 原始协议

use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUpstreamInfo {
    pub discovered: bool,
    pub node: Option<UpstreamNode>,
    pub observed_at: Option<DateTime<Utc>>,
    pub stale: bool,
    pub connection_revision: String,
    pub sync: UpstreamSync,
    pub last_error: Option<String>,
    pub pool: Option<UpstreamPool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamNode {
    pub id: String,
    pub name: String,
    pub software: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamSyncState {
    Idle,
    Queued,
    Running,
}

impl UpstreamSyncState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Queued => "queued",
            Self::Running => "running",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamSync {
    pub state: UpstreamSyncState,
    pub request_generation: u64,
    pub completed_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamRefreshReceipt {
    pub account_id: String,
    pub connection_revision: String,
    pub request_generation: u64,
    pub state: UpstreamSyncState,
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
