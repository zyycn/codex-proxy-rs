use chrono::Utc;
use gateway_store::postgres::{OpsEvent, OpsEventLevel};

#[test]
fn request_scoped_ops_event_requires_attempt_index() {
    let event = OpsEvent {
        id: "event-1".to_owned(),
        model_request_id: Some("request-1".to_owned()),
        attempt_index: None,
        level: OpsEventLevel::Warning,
        component: "routing".to_owned(),
        operation: "fallback".to_owned(),
        provider_instance_id: None,
        provider_kind: None,
        provider_account_id: None,
        provider_account_ref: None,
        upstream_model_id: None,
        failure_kind: "timeout".to_owned(),
        status_code: None,
        provider_error_code: None,
        retry_after_ms: None,
        upstream_request_id: None,
        latency_ms: None,
        message: "safe".to_owned(),
        occurrence_count: 1,
        created_at: Utc::now(),
    };
    assert!(event.validate().is_err());
}
