use gateway_api::admin::system::{UpdateDetailQuery, UpdateRequest};

#[test]
fn update_detail_query_should_default_to_no_refresh_and_reject_unknown_fields() {
    let query: UpdateDetailQuery = serde_json::from_str("{}").expect("empty query object");
    assert!(!query.refresh());
    assert!(serde_json::from_str::<UpdateDetailQuery>(r#"{"refresh":true,"extra":1}"#).is_err());
}

#[test]
fn update_request_should_preserve_target_version_for_domain_validation() {
    let request: UpdateRequest =
        serde_json::from_str(r#"{"targetVersion":"v0.2.0"}"#).expect("update request");
    assert_eq!(request.into_target_version(), "v0.2.0");
    assert!(
        serde_json::from_str::<UpdateRequest>(r#"{"targetVersion":"v0.2.0","extra":1}"#).is_err()
    );
}
