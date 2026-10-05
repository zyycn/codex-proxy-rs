//! 插件线协议错误码、上游发送状态与安全错误对象

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unsupported,
    Rejected,
    InvalidInput,
    PermissionDenied,
    Upstream,
    Fault,
    Timeout,
    Cancelled,
    Uncertain,
    Capacity,
    Conflict,
}

/// 发送事实由宿主取单调上界，不能由插件降回未发送
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendState {
    NotSent,
    Sent,
    Ambiguous,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginFault {
    pub code: ErrorCode,
    pub message: String,
    pub send_state: SendState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// 原始边界错误详情；按合同完整传递，不作为诊断日志输出
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl std::fmt::Debug for PluginFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginFault")
            .field("code", &self.code)
            .field("send_state", &self.send_state)
            .field("http_status", &self.http_status)
            .finish_non_exhaustive()
    }
}

impl PluginFault {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            send_state: SendState::NotSent,
            http_status: None,
            details: None,
        }
    }
}
