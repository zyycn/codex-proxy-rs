//! CPR 互联的版本化只读线协议，不包含授权、采集或调度策略

use serde::{Deserialize, Serialize};

pub const SHARING_DISABLED_CODE: &str = "peer_sharing_disabled";
pub const OBJECT: &str = "cpr.peer";
pub const SCHEMA_VERSION: u32 = 3;
pub const MAX_ACCOUNTS: usize = 3_000;
pub const MAX_QUOTA_WINDOWS: usize = 6_000;
pub const MAX_MODELS: usize = 2_000;
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// 可选能力按当前认证主体公开，未知能力可以忽略
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerLink {
    pub object: String,
    pub schema_version: u32,
    pub capabilities: Vec<String>,
    pub observed_at: String,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<PeerUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_limits: Option<PeerKeyLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<PeerPool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<PeerQuota>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<PeerModelAvailability>,
}

/// 当前认证密钥公开的请求上限，缺失表示未知，零表示不限
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerKeyLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_per_minute: Option<u64>,
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
    /// 仅在提供金额窗口时必填；访问结论不依赖金额或单位
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// 上游账本给出的访问结论，缺失时不能根据展示金额推断恢复
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed: Option<bool>,
    /// 只返回实际提供的账本窗口，缺失周期不表示零用量或不限额
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<PeerBudgetWindow>,
}

/// 金额保留十进制字符串，不限额时 total 与 remaining 为 null
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerBudgetWindow {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_reached: Option<bool>,
    pub total: Option<String>,
    pub used: String,
    pub remaining: Option<String>,
    pub reset_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PeerPool {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    pub account_count: u32,
    pub states: PeerPoolStates,
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

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PeerQuota {
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
    /// 缺失时使用 general 分组；额度桶身份优先由 limit_id 表达
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// 缺失时以稳定的窗口 key 展示
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
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
