//! 管理端日志处理器。

use axum::http::StatusCode;
use codex_proxy_core::events::model::EventLevel;
use codex_proxy_runtime::services::{AdminLogError, AdminLogState};
use serde::Serialize;

use crate::admin_api::AdminError;

pub mod detail;
pub mod query;
pub mod state;

pub use detail::log_detail;
pub use query::logs;
pub use state::{clear_logs, logs_state, update_logs_state};

/// 管理端日志状态响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogStateData {
    /// 是否启用。
    pub enabled: bool,
    /// 内存容量。
    pub capacity: u32,
    /// 是否捕获请求体。
    pub capture_body: bool,
    /// 已存储数量。
    pub stored_count: u64,
}

/// 清空日志响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearLogsData {
    /// 清理数量。
    pub cleared: u64,
}

impl From<AdminLogState> for LogStateData {
    fn from(state: AdminLogState) -> Self {
        Self {
            enabled: state.enabled,
            capacity: state.capacity,
            capture_body: state.capture_body,
            stored_count: state.stored_count,
        }
    }
}

fn log_error(error: AdminLogError, request_id: String) -> AdminError {
    match error {
        AdminLogError::List
        | AdminLogError::Get
        | AdminLogError::Count
        | AdminLogError::Clear
        | AdminLogError::Append
        | AdminLogError::Trim => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            error.to_string(),
            request_id,
        ),
        AdminLogError::InvalidCapacity => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            error.to_string(),
            request_id,
        ),
    }
}

fn level_from_query(value: Option<String>) -> Result<Option<EventLevel>, String> {
    let Some(value) = non_empty(value) else {
        return Ok(None);
    };
    match value.as_str() {
        "debug" => Ok(Some(EventLevel::Debug)),
        "info" => Ok(Some(EventLevel::Info)),
        "warn" => Ok(Some(EventLevel::Warn)),
        "error" => Ok(Some(EventLevel::Error)),
        other => Err(format!("Unsupported log level: {other}")),
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
