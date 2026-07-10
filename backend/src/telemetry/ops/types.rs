//! 运维错误事件模型。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

/// 结构化运维错误事件。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OpsErrorLog {
    /// 事件 ID。
    pub id: String,
    /// 请求 ID。
    pub request_id: Option<String>,
    /// 调用方客户端 API key 的稳定 ID。
    pub client_api_key_id: Option<String>,
    /// 事件类别。
    pub kind: String,
    /// 已知的上游 provider；调度前失败可为空。
    pub provider: Option<String>,
    /// 账号 ID。预账号调度失败时为空。
    pub account_id: Option<String>,
    /// HTTP 路由。
    pub route: Option<String>,
    /// 模型名。
    pub model: Option<String>,
    /// 事件状态码。
    pub status_code: Option<i64>,
    /// 客户端看到的状态码。
    pub client_status_code: Option<i64>,
    /// 上游 HTTP 状态码。
    pub upstream_status_code: Option<i64>,
    /// 上游传输方式，例如 http_sse 或 websocket。
    pub transport: Option<String>,
    /// 同一请求内的上游尝试序号。
    pub attempt_index: Option<i64>,
    /// 失败分类。
    pub failure_class: Option<String>,
    /// 上游响应 ID。
    pub response_id: Option<String>,
    /// 上游请求 ID。
    pub upstream_request_id: Option<String>,
    /// 仅用于错误时间桶维度，不落 ops_error_logs 明细列。
    #[serde(skip)]
    pub service_tier: Option<String>,
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

impl OpsErrorLog {
    /// 构造运维错误事件。
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id: format!("ops_error_{}", Uuid::new_v4().simple()),
            request_id: None,
            client_api_key_id: None,
            kind: kind.into(),
            provider: None,
            account_id: None,
            route: None,
            model: None,
            status_code: None,
            client_status_code: None,
            upstream_status_code: None,
            transport: None,
            attempt_index: None,
            failure_class: None,
            response_id: None,
            upstream_request_id: None,
            service_tier: None,
            latency_ms: None,
            message: message.into(),
            metadata: Value::Object(Map::default()),
            created_at: Utc::now(),
        }
    }
}
