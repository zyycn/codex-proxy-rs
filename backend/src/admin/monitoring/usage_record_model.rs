//! 使用记录模型。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

/// 事件等级。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UsageRecordLevel {
    /// 调试。
    Debug,
    /// 信息。
    Info,
    /// 警告。
    Warn,
    /// 错误。
    Error,
}

/// 结构化使用记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    /// 事件 ID。
    pub id: String,
    /// 请求 ID。
    pub request_id: Option<String>,
    /// 事件类别。
    pub kind: String,
    /// 事件等级。
    pub level: UsageRecordLevel,
    /// 账号 ID。
    pub account_id: Option<String>,
    /// HTTP 路由。
    pub route: Option<String>,
    /// 模型名。
    pub model: Option<String>,
    /// HTTP 状态码。
    pub status_code: Option<i64>,
    /// 上游传输方式，例如 http_sse 或 websocket。
    pub transport: Option<String>,
    /// 同一请求内的上游尝试序号。
    pub attempt_index: Option<i64>,
    /// 上游 HTTP 状态码。
    pub upstream_status_code: Option<i64>,
    /// 失败分类。
    pub failure_class: Option<String>,
    /// 上游响应 ID。
    pub response_id: Option<String>,
    /// 上游请求 ID。
    pub upstream_request_id: Option<String>,
    /// 请求耗时毫秒。
    pub latency_ms: Option<i64>,
    /// 展示消息。
    pub message: String,
    /// 结构化元数据。
    pub metadata: Value,
    /// 创建时间。
    #[serde(serialize_with = "crate::infra::time::serialize_china_rfc3339")]
    pub created_at: DateTime<Utc>,
}

impl UsageRecord {
    /// 构造使用记录。
    pub fn new(
        kind: impl Into<String>,
        level: UsageRecordLevel,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id: format!("usage_{}", Uuid::new_v4().simple()),
            request_id: None,
            kind: kind.into(),
            level,
            account_id: None,
            route: None,
            model: None,
            status_code: None,
            transport: None,
            attempt_index: None,
            upstream_status_code: None,
            failure_class: None,
            response_id: None,
            upstream_request_id: None,
            latency_ms: None,
            message: message.into(),
            metadata: Value::Object(Map::default()),
            created_at: Utc::now(),
        }
    }
}

pub(in crate::admin) fn metadata_service_tier(metadata: &Value) -> Option<&str> {
    metadata
        .get("serviceTier")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(in crate::admin) fn metadata_string(metadata: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        metadata
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

pub(in crate::admin) fn metadata_i64(metadata: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| metadata.get(*key).and_then(Value::as_i64))
}

pub(in crate::admin) fn metadata_nonnegative_i64(metadata: &Value, keys: &[&str]) -> Option<i64> {
    metadata_i64(metadata, keys).filter(|value| *value >= 0)
}

/// 响应事件记录（供 dispatch 模块使用）。
pub struct ResponseUsageRecord<'a> {
    pub usage_records: &'a crate::admin::monitoring::usage_record_service::AdminUsageRecordService,
    pub request_id: &'a str,
    pub account_id: &'a str,
    pub route: &'a str,
    pub model: &'a str,
    pub requested_model: Option<&'a str>,
    pub client_ip: Option<&'a str>,
    pub client_user_agent: Option<&'a str>,
    pub reasoning_effort: Option<&'a str>,
    pub service_tier: Option<&'a str>,
    pub started_at: std::time::Instant,
    pub status_code: i64,
    pub level: UsageRecordLevel,
    pub message: &'a str,
    pub metadata: serde_json::Value,
    pub rate_limit_headers: &'a [(String, String)],
}
