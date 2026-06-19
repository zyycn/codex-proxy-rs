//! 用量聚合模型。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 用量聚合窗口。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    /// 窗口开始时间。
    pub started_at: DateTime<Utc>,
    /// 请求数。
    pub request_count: u64,
    /// 输入 token 数。
    pub input_tokens: u64,
    /// 输出 token 数。
    pub output_tokens: u64,
}

/// 用量快照。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    /// 账号 ID。
    pub account_id: String,
    /// 当前窗口。
    pub window: UsageWindow,
}
