//! 事件日志模型、端口与策略服务。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
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
            transport: None,
            attempt_index: None,
            upstream_status_code: None,
            failure_class: None,
            response_id: None,
            upstream_request_id: None,
            latency_ms: None,
            message: message.into(),
            metadata: Value::Object(Default::default()),
            created_at: Utc::now(),
        }
    }
}

/// 事件日志存储错误。
#[derive(Debug, Error)]
pub enum EventLogStoreError {
    /// 底层存储失败。
    #[error("event log store operation failed: {message}")]
    OperationFailed {
        /// 错误说明。
        message: String,
    },
}

/// 事件日志存储结果。
pub type EventLogStoreResult<T> = Result<T, EventLogStoreError>;

/// 事件日志存储端口。
#[async_trait]
pub trait EventLogStore: Send + Sync + 'static {
    /// 写入事件日志。
    async fn append(&self, event: &EventLog) -> EventLogStoreResult<()>;
}

/// 事件日志服务。
#[derive(Debug, Clone)]
pub struct EventLogService {
    enabled: bool,
}

impl EventLogService {
    /// 构造事件日志服务。
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    /// 判断事件是否应该记录。
    pub fn should_record(&self, event: &EventLog) -> bool {
        self.enabled || event.level == EventLevel::Error
    }
}

/// 响应事件记录（供 dispatch 模块使用）。
pub struct ResponseEventRecord<'a> {
    pub logs: &'a crate::admin::monitoring::event_store::AdminLogService,
    pub request_id: &'a str,
    pub account_id: &'a str,
    pub route: &'a str,
    pub model: &'a str,
    pub started_at: std::time::Instant,
    pub status_code: i64,
    pub level: EventLevel,
    pub message: &'a str,
    pub metadata: serde_json::Value,
    pub rate_limit_headers: &'a [(String, String)],
}
