//! CPR 互联的版本化只读线协议，不包含授权、采集或调度策略

use serde::{Deserialize, Serialize};

pub const OBJECT: &str = "cpr.peer";
pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_ACCOUNTS: usize = 3_000;
pub const MAX_QUOTA_WINDOWS: usize = 6_000;
pub const MAX_MODELS: usize = 2_000;
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// 可选能力按当前认证主体公开，未知能力可以忽略
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerLink {
    pub object: String,
    pub schema_version: u32,
    pub node: PeerNode,
    pub capabilities: Vec<String>,
    pub observed_at: String,
    pub expires_at: String,
    pub usage: PeerUsage,
    pub pool: PeerPool,
    #[serde(default)]
    pub quota: PeerQuota,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<PeerModelAvailability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerNode {
    pub id: String,
    pub name: String,
    pub software: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerComponentStatus {
    Available,
    Unavailable,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerUsage {
    pub status: PeerComponentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<PeerKeyUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerKeyUsage {
    pub unit: String,
    pub daily: PeerBudgetWindow,
    pub weekly: PeerBudgetWindow,
}

/// 金额保留十进制字符串，不限额时 total 与 remaining 为 null
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerBudgetWindow {
    pub total: Option<String>,
    pub used: String,
    pub remaining: Option<String>,
    #[serde(rename = "resetsAt")]
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerPoolDisclosure {
    Summary,
    #[default]
    #[serde(other)]
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PeerPool {
    pub disclosure: PeerPoolDisclosure,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub states: Option<PeerPoolStates>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plans: Option<Vec<PeerPlanCount>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PeerPoolStates {
    pub ready: u32,
    pub cooldown: u32,
    pub exhausted: u32,
    pub disabled: u32,
    pub error: u32,
    pub unknown: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerPlanCount {
    pub name: String,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerQuotaDisclosure {
    Accounts,
    #[default]
    #[serde(other)]
    None,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PeerQuota {
    pub disclosure: PeerQuotaDisclosure,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<PeerQuotaAccount>,
}

/// 匿名标识在授权 Key 内稳定，窗口仍保留账号与模型归属
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerQuotaAccount {
    pub id: String,
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub windows: Vec<PeerQuotaWindow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerQuotaWindow {
    pub key: String,
    pub group: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<String>,
    #[serde(default)]
    pub limit_reached: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerModelCoverage {
    Complete,
    #[serde(other)]
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerModelState {
    Available,
    Exhausted,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerModelAvailability {
    pub model: String,
    pub coverage: PeerModelCoverage,
    pub state: PeerModelState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}
