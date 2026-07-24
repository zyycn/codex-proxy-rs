use chrono::Utc;
use gateway_store::postgres::ModelRequestHistoryRecord;

#[test]
fn history_contains_no_payload_field() {
    let history = ModelRequestHistoryRecord {
        id: "request-1".to_owned(),
        client_api_key_ref: "key-1".to_owned(),
        requested_model_id: "coding".to_owned(),
        provider_kind: None,
        provider_account_ref: None,
        upstream_model_id: None,
        outcome: "failed".to_owned(),
        client_response_id: None,
        upstream_response_id: None,
        started_at: Utc::now(),
        completed_at: None,
    };

    assert_eq!(history.id, "request-1");
}
