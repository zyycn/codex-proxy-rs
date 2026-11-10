//! 验证管理端错误位置的证据优先级与未知边界

use gateway_admin::model::observability::{RequestOutcome, UsageErrorOrigin};

use super::{Utc, total_record};

#[test]
fn usage_error_origin_does_not_infer_from_client_status_or_message() {
    let mut record = total_record("unknown", Some("openai"), "unavailable", Utc::now());
    record.client_status_code = Some(502);
    record.upstream_status_code = Some(200);
    record.error_message = Some("Upstream error: connection reset".to_owned());
    // 这些分类既可能来自本地校验、选凭据、RPC、准入或取消，也可能来自上游
    for kind in [
        "invalid_request",
        "unsupported",
        "unauthorized",
        "rate_limited",
        "quota_exhausted",
        "timeout",
        "cancelled",
        "protocol",
        "unavailable",
        "upstream_unavailable",
        "continuation_recovery_required",
        "future_error_kind",
    ] {
        record.error_kind = Some(kind.to_owned());
        assert_eq!(record.error_origin(), UsageErrorOrigin::Unknown, "{kind}");
    }
}

#[test]
fn usage_error_origin_recognizes_unambiguous_failure_kinds() {
    use UsageErrorOrigin::{Connection, Proxy, Upstream};

    let mut record = total_record("classified", Some("openai"), "unavailable", Utc::now());
    record.outcome = RequestOutcome::Failed;
    for (kind, expected) in [
        // 本地执行、选路、账号与准入策略已经确定了错误位置
        ("internal_error", Proxy),
        ("model_not_found", Proxy),
        ("no_available_provider", Proxy),
        ("no_eligible_account", Proxy),
        ("account_capacity_unavailable", Proxy),
        ("middleware_capacity_unavailable", Proxy),
        ("concurrency_queue_full", Proxy),
        ("concurrency_queue_timeout", Proxy),
        ("provider_infrastructure_unavailable", Proxy),
        ("session_policy_restricted", Proxy),
        ("request_policy_denied", Proxy),
        ("policy_denied", Proxy),
        ("budget_exhausted", Proxy),
        ("process_interrupted", Proxy),
        ("process_terminated", Proxy),
        // Provider 的权限拒绝、消息过大和模型容量拒绝来自明确上游拒绝
        ("permission_denied", Upstream),
        ("message_too_big", Upstream),
        ("upstream_capacity_unavailable", Upstream),
        ("transport", Connection),
    ] {
        record.error_kind = Some(kind.to_owned());
        assert_eq!(record.error_origin(), expected, "{kind}");
    }
}

#[test]
fn usage_error_origin_distinguishes_upstream_and_local_size_rejections() {
    let mut record = total_record("size", Some("openai"), "unavailable", Utc::now());
    record.outcome = RequestOutcome::Failed;
    // 收到上游 close 1009 才产生 MessageTooBig；本地发送前超限保存续接拒绝原因
    record.error_kind = Some("message_too_big".to_owned());
    assert_eq!(record.error_origin(), UsageErrorOrigin::Upstream);
    record.error_kind = Some("continuation_recovery_required".to_owned());
    record.continuation_unavailable_reason = Some("transport_payload_too_large".to_owned());
    assert_eq!(record.error_origin(), UsageErrorOrigin::Proxy);
    record.continuation_unavailable_reason = None;
    record.error_message = Some("message too big".to_owned());
    assert_eq!(record.error_origin(), UsageErrorOrigin::Unknown);
}

#[test]
fn usage_error_origin_prefers_upstream_facts_to_local_classification() {
    let mut record = total_record("upstream", Some("openai"), "unavailable", Utc::now());
    record.error_kind = Some("internal_error".to_owned());
    record.continuation_unavailable_reason = Some("reused_connection_lost".to_owned());
    record.client_status_code = Some(503);
    record.upstream_status_code = Some(429);
    assert_eq!(record.error_origin(), UsageErrorOrigin::Upstream);
    record.upstream_status_code = Some(200);
    record.error_code = Some("previous_response_not_found".to_owned());
    assert_eq!(record.error_origin(), UsageErrorOrigin::Upstream);
    record.error_code = None;
    record.websocket_close_code = Some(1011);
    assert_eq!(record.error_origin(), UsageErrorOrigin::Upstream);
    record.websocket_close_code = None;
    record.has_upstream_error = true;
    assert_eq!(record.error_origin(), UsageErrorOrigin::Upstream);
}

#[test]
fn usage_error_origin_uses_connection_exit_boundaries() {
    use UsageErrorOrigin::{Connection, Proxy, Unknown, Upstream};

    let mut record = total_record("connection", Some("openai"), "unavailable", Utc::now());
    record.outcome = RequestOutcome::Failed;
    // Provider 的传输和协议错误会统一保存为 upstream_unavailable，具体退出原因仍独立保存
    record.error_kind = Some("upstream_unavailable".to_owned());
    record.upstream_status_code = Some(200);
    for (reason, expected) in [
        // 本地通道、下游交付与连接池回收，不表示客户端取消应由代理承担责任
        ("command_channel_closed", Proxy),
        ("local_close", Proxy),
        ("message_receiver_closed", Proxy),
        ("client_cancelled", Proxy),
        ("downstream_send_failed", Proxy),
        ("downstream_connection_closed", Proxy),
        ("downstream_rebound", Proxy),
        ("pool_shutdown", Proxy),
        ("max_age_expired", Proxy),
        ("idle_timeout_expired", Proxy),
        ("proxy_capacity_evicted", Proxy),
        ("search_activation_required", Proxy),
        // 已收到明确的上游帧或终态，与无关闭帧的 EOF 区分
        ("upstream_close", Upstream),
        ("normal_close", Upstream),
        ("unexpected_binary_event", Upstream),
        ("incomplete_response", Upstream),
        ("failed_response", Upstream),
        ("eof", Connection),
        ("unexpected_eof", Connection),
        ("transport_error", Connection),
        ("connection_reset", Connection),
        ("connection_aborted", Connection),
        ("broken_pipe", Connection),
        ("tcp_reset", Connection),
        ("reset_without_closing_handshake", Connection),
        ("transport_timeout", Connection),
        ("outbound_transport_error", Connection),
        ("receive_idle_timeout", Connection),
        ("pong_timeout", Connection),
        ("ping_write_timeout", Connection),
        ("pong_write_timeout", Connection),
        // stream_eof 也可能是本地收包任务退出，笼统退出摘要合并了多种底层失败
        ("stream_eof", Unknown),
        ("upstream_closed", Unknown),
        ("upstream_receive_failed", Unknown),
        ("exchange_failure", Unknown),
        ("running", Unknown),
        ("future_exit_reason", Unknown),
    ] {
        record.upstream_connection_exit_reason = Some(reason.to_owned());
        assert_eq!(record.error_origin(), expected, "{reason}");
    }
}

#[test]
fn usage_error_origin_distinguishes_local_policy_from_connection_failure() {
    let mut record = total_record("local", Some("openai"), "unavailable", Utc::now());
    record.client_status_code = Some(503);
    record.upstream_status_code = Some(200);
    record.error_kind = Some("budget_exhausted".to_owned());
    assert_eq!(record.error_origin(), UsageErrorOrigin::Proxy);
    record.error_kind = Some("transport".to_owned());
    record.upstream_connection_exit_reason = Some("unexpected_eof".to_owned());
    assert_eq!(record.error_origin(), UsageErrorOrigin::Connection);
    record.continuation_unavailable_reason = Some("latest_response_mismatch".to_owned());
    assert_eq!(record.error_origin(), UsageErrorOrigin::Proxy);
    record.continuation_unavailable_reason = Some("upstream_rejected".to_owned());
    assert_eq!(record.error_origin(), UsageErrorOrigin::Upstream);
    record.continuation_unavailable_reason = None;
    record.upstream_connection_exit_reason = Some("upstream_close".to_owned());
    assert_eq!(record.error_origin(), UsageErrorOrigin::Upstream);
}
