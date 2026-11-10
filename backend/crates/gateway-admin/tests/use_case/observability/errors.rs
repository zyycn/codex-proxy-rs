//! 验证管理端错误位置的证据优先级与未知边界

use gateway_admin::model::observability::UsageErrorOrigin;

use super::{Utc, total_record};

#[test]
fn usage_error_origin_does_not_infer_from_client_status_or_message() {
    let mut record = total_record("unknown", Some("openai"), "unavailable", Utc::now());
    record.client_status_code = Some(502);
    record.upstream_status_code = Some(200);
    record.error_message = Some("Upstream error: connection reset".to_owned());
    record.error_kind = Some("future_error_kind".to_owned());
    assert_eq!(record.error_origin(), UsageErrorOrigin::Unknown);
    record.error_kind = Some("timeout".to_owned());
    assert_eq!(record.error_origin(), UsageErrorOrigin::Unknown);
}

#[test]
fn usage_error_origin_prefers_upstream_facts_to_local_classification() {
    let mut record = total_record("upstream", Some("openai"), "unavailable", Utc::now());
    record.error_kind = Some("budget_exhausted".to_owned());
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
