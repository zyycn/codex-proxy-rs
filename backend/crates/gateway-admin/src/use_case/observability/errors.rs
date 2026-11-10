//! 从已保存的错误事实投影管理端来源，不参与执行错误分类或重试

use crate::model::observability::{UsageErrorOrigin, UsageListRecord, UsageRecord};

impl UsageListRecord {
    #[must_use]
    pub fn error_origin(&self) -> UsageErrorOrigin {
        error_origin(
            self.error_code.as_deref(),
            self.websocket_close_code,
            self.upstream_status_code,
            self.error_kind.as_deref(),
            self.continuation_unavailable_reason.as_deref(),
            self.upstream_connection_exit_reason.as_deref(),
        )
    }
}

impl UsageRecord {
    #[must_use]
    pub fn error_origin(&self) -> UsageErrorOrigin {
        if self.upstream_error.as_ref().is_some_and(|error| {
            [&error.message, &error.code, &error.error_type]
                .into_iter()
                .any(|value| value.as_ref().is_some_and(|value| !value.is_empty()))
        }) {
            return UsageErrorOrigin::Upstream;
        }
        error_origin(
            self.provider_error_code.as_deref(),
            self.websocket_close_code,
            self.upstream_status_code,
            self.error_kind.as_deref(),
            self.continuation_unavailable_reason.as_deref(),
            self.upstream_connection_exit_reason.as_deref(),
        )
    }
}

// 上游原始事实优先；客户端 HTTP 状态和任意错误文案不能证明错误来源
fn error_origin(
    upstream_code: Option<&str>,
    websocket_close_code: Option<u16>,
    upstream_status_code: Option<u16>,
    error_kind: Option<&str>,
    continuation_reason: Option<&str>,
    connection_exit_reason: Option<&str>,
) -> UsageErrorOrigin {
    if upstream_code.is_some_and(|code| !code.is_empty())
        || websocket_close_code.is_some()
        || upstream_status_code.is_some_and(|status| status >= 400)
        || matches!(
            continuation_reason,
            Some("upstream_rejected" | "quota_exhausted")
        )
        || error_kind == Some("upstream_capacity_unavailable")
    {
        return UsageErrorOrigin::Upstream;
    }
    if matches!(
        continuation_reason,
        Some(
            "scope_unavailable"
                | "pool_unavailable"
                | "fresh_connection_required"
                | "connection_busy"
                | "latest_response_mismatch"
                | "reused_connection_lost"
        )
    ) || matches!(
        error_kind,
        Some(
            "process_interrupted"
                | "process_terminated"
                | "no_available_provider"
                | "no_eligible_account"
                | "account_capacity_unavailable"
                | "middleware_capacity_unavailable"
                | "concurrency_queue_full"
                | "concurrency_queue_timeout"
                | "provider_infrastructure_unavailable"
                | "session_policy_restricted"
                | "request_policy_denied"
                | "policy_denied"
                | "budget_exhausted"
        )
    ) {
        return UsageErrorOrigin::Proxy;
    }
    if matches!(
        connection_exit_reason,
        Some("upstream_close" | "normal_close")
    ) {
        return UsageErrorOrigin::Upstream;
    }
    if error_kind == Some("transport")
        || matches!(
            connection_exit_reason,
            Some(
                "eof"
                    | "unexpected_eof"
                    | "transport_error"
                    | "connection_reset"
                    | "tcp_reset"
                    | "reset_without_closing_handshake"
                    | "pong_timeout"
                    | "ping_write_timeout"
                    | "pong_write_timeout"
            )
        )
    {
        return UsageErrorOrigin::Connection;
    }
    UsageErrorOrigin::Unknown
}
