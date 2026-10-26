//! 入口认证、权限与账号额度事实，以及主动额度刷新合同
//! 响应忽略未知字段，以兼容宿主新增事实

use serde::{Deserialize, Serialize};

pub const ACCOUNTS_LIST: &str = "host.data.accounts.list";
pub const QUOTA_REFRESH: &str = "host.quota_observations.refresh";
pub const QUOTA_GET: &str = "host.data.quota.get";
pub const KEYS_GET: &str = "host.data.keys.get";
pub const CLIENT_VERIFY: &str = "host.client.verify";
pub const ACCESS_CHECK: &str = "host.data.access.check";
pub const QUOTA_DETAILS: &str = "host.data.quota.details";

/// 交给宿主入口认证计划的凭据；只通过二进制载荷传输，不提供内容型 Debug
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyClientRequest {
    pub authorization: String,
}

/// 宿主认证与 principal 映射后的执行身份，不包含 Key 明文
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedClient {
    pub client_key_id: String,
}

/// 候选账号的冻结权限查询；上游模型名不解析为公开模型别名
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessQuery {
    pub client_key_id: String,
    pub account_ids: Vec<String>,
    pub upstream_model: Option<String>,
}

/// 授权结果不代表账号健康、额度或执行准入结果
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessFacts {
    pub schema_version: u32,
    pub client_key_id: String,
    /// 与查询候选保持相同顺序
    pub accounts: Vec<AccountAccessFacts>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountAccessFacts {
    pub account_id: String,
    pub allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientKeyFactsQuery {
    pub client_key_id: String,
}

/// 当前显式分组绑定；空列表不是单账号范围，不包含密钥或凭据
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientKeyFacts {
    pub schema_version: u32,
    pub client_key_id: String,
    pub enabled: bool,
    pub group_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountFactsQuery {
    pub provider_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountFacts {
    pub account_id: String,
    pub provider_id: String,
    pub name: String,
    pub email: Option<String>,
    pub group_ids: Vec<String>,
    pub enabled: bool,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Provider 已有快照的必要投影；空观测时间表示没有可用样本，不代表额度为零
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaFacts {
    pub schema_version: u32,
    pub account_id: String,
    pub observed_at_ms: Option<i64>,
    pub windows: Vec<QuotaWindowFacts>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaWindowFacts {
    pub key: String,
    pub window_seconds: Option<u64>,
    /// 百分比而非 0～1 比率；未知值为 null
    pub used_percent: Option<f64>,
    pub reset_at_ms: Option<i64>,
}

/// Provider 已解析的窗口与点数事实，不包含原始 Provider 文档或宿主预测
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaDetails {
    pub schema_version: u32,
    pub account_id: String,
    pub observed_at_ms: Option<i64>,
    pub limit_reached: bool,
    pub windows: Vec<QuotaWindowDetails>,
    pub credits: Option<QuotaCredits>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaWindowDetails {
    pub key: String,
    pub group: String,
    pub label: String,
    pub limit_id: Option<String>,
    pub limit_name: Option<String>,
    pub role: Option<String>,
    pub window_seconds: Option<u64>,
    /// 百分比而非 0～1 比率；未知值为 null
    pub used_percent: Option<f64>,
    pub reset_at_ms: Option<i64>,
    pub limit_reached: bool,
}

/// 余额沿用上游的十进制文本，不转为浮点数或把未知值解释为零
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaCredits {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}
