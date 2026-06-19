//! 事件日志模型。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// 事件等级。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EventLevel {
    /// 调试。
    Debug,
    /// 信息。
    Info,
    /// 警告。
    Warn,
    /// 错误。
    Error,
}

/// 结构化事件日志。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EventLog {
    /// 事件 ID。
    pub id: String,
    /// 请求 ID。
    pub request_id: Option<String>,
    /// 事件类别。
    pub kind: String,
    /// 事件等级。
    pub level: EventLevel,
    /// 账号 ID。
    pub account_id: Option<String>,
    /// HTTP 路由。
    pub route: Option<String>,
    /// 模型名。
    pub model: Option<String>,
    /// HTTP 状态码。
    pub status_code: Option<i64>,
    /// 请求耗时毫秒。
    pub latency_ms: Option<i64>,
    /// 展示消息。
    pub message: String,
    /// 结构化元数据。
    pub metadata: Value,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

impl EventLog {
    /// 构造事件日志。
    pub fn new(kind: impl Into<String>, level: EventLevel, message: impl Into<String>) -> Self {
        Self {
            id: format!("log_{}", Uuid::new_v4().simple()),
            request_id: None,
            kind: kind.into(),
            level,
            account_id: None,
            route: None,
            model: None,
            status_code: None,
            latency_ms: None,
            message: message.into(),
            metadata: Value::Object(Default::default()),
            created_at: Utc::now(),
        }
    }
}
