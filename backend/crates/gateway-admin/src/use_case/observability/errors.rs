//! 从已保存的错误事实投影管理端来源，不参与执行错误分类或重试

use crate::model::observability::{
    UsageErrorEvidence, UsageErrorOrigin, UsageListRecord, UsageRecord,
};

impl UsageListRecord {
    #[must_use]
    pub fn error_origin(&self) -> UsageErrorOrigin {
        error_origin(
            self.has_upstream_error,
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
        let has_upstream_error = self.upstream_error.as_ref().is_some_and(|error| {
            [&error.message, &error.code, &error.error_type]
                .into_iter()
                .any(|value| value.as_ref().is_some_and(|value| !value.is_empty()))
        });
        error_origin(
            has_upstream_error,
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
    has_upstream_error: bool,
    upstream_code: Option<&str>,
    websocket_close_code: Option<u16>,
    upstream_status_code: Option<u16>,
    error_kind: Option<&str>,
    continuation_reason: Option<&str>,
    connection_exit_reason: Option<&str>,
) -> UsageErrorOrigin {
    use UsageErrorEvidence::*;
    UsageErrorOrigin::RULES
        .iter()
        .find(|(evidence, _)| match evidence {
            ProviderErrorCode => upstream_code.is_some_and(|code| !code.is_empty()),
            UpstreamStatusAtLeast(min) => upstream_status_code.is_some_and(|status| status >= *min),
            SavedUpstreamErrorOrClose => has_upstream_error || websocket_close_code.is_some(),
            ErrorKind(values) => error_kind.is_some_and(|value| values.contains(&value)),
            Continuation(values) => {
                continuation_reason.is_some_and(|value| values.contains(&value))
            }
            ConnectionExit(values) => {
                connection_exit_reason.is_some_and(|value| values.contains(&value))
            }
        })
        .map_or(UsageErrorOrigin::Unknown, |(_, origin)| *origin)
}

impl UsageErrorOrigin {
    /// 第一条命中的规则决定来源；含义不唯一的分类在缺少其他证据时保留 unknown
    pub const RULES: &'static [(UsageErrorEvidence, Self)] = {
        use UsageErrorEvidence::*;
        &[
            (ProviderErrorCode, Self::Upstream),
            (UpstreamStatusAtLeast(400), Self::Upstream),
            (
                Continuation(&["upstream_rejected", "quota_exhausted"]),
                Self::Upstream,
            ),
            // message_too_big 只由收到的上游 close 1009 产生
            (
                ErrorKind(&[
                    "permission_denied",
                    "message_too_big",
                    "upstream_capacity_unavailable",
                ]),
                Self::Upstream,
            ),
            (SavedUpstreamErrorOrClose, Self::Upstream),
            (
                Continuation(&[
                    "scope_unavailable",
                    "pool_unavailable",
                    "fresh_connection_required",
                    "connection_busy",
                    "latest_response_mismatch",
                    "reused_connection_lost",
                    "transport_payload_too_large",
                ]),
                Self::Proxy,
            ),
            (
                ErrorKind(&[
                    "process_interrupted",
                    "process_terminated",
                    "internal_error",
                    "model_not_found",
                    "no_available_provider",
                    "no_eligible_account",
                    "account_capacity_unavailable",
                    "middleware_capacity_unavailable",
                    "concurrency_queue_full",
                    "concurrency_queue_timeout",
                    "provider_infrastructure_unavailable",
                    "session_policy_restricted",
                    "request_policy_denied",
                    "policy_denied",
                    "budget_exhausted",
                ]),
                Self::Proxy,
            ),
            (
                ConnectionExit(&[
                    "command_channel_closed",
                    "local_close",
                    "message_receiver_closed",
                    "client_cancelled",
                    "downstream_send_failed",
                    "downstream_connection_closed",
                    "downstream_rebound",
                    "pool_shutdown",
                    "max_age_expired",
                    "idle_timeout_expired",
                    "proxy_capacity_evicted",
                    "search_activation_required",
                ]),
                Self::Proxy,
            ),
            (
                ConnectionExit(&[
                    "upstream_close",
                    "normal_close",
                    "unexpected_binary_event",
                    "incomplete_response",
                    "failed_response",
                ]),
                Self::Upstream,
            ),
            (ErrorKind(&["transport"]), Self::Connection),
            (
                ConnectionExit(&[
                    "eof",
                    "unexpected_eof",
                    "transport_error",
                    "connection_reset",
                    "connection_aborted",
                    "broken_pipe",
                    "tcp_reset",
                    "reset_without_closing_handshake",
                    "transport_timeout",
                    "outbound_transport_error",
                    "receive_idle_timeout",
                    "pong_timeout",
                    "ping_write_timeout",
                    "pong_write_timeout",
                ]),
                Self::Connection,
            ),
        ]
    };
}
