//! 验证宿主回调的状态、账号、模型与二进制载荷合同

use gateway_plugin_sdk::call::host::{
    AffinityLookupRequest, AuthCredential, AuthListRequest, AuthSaveRequest, LogLevel, LogRequest,
    LogResult, ModelEventBatch, ModelExecuteRequest, ModelOperation, StateDeleteRequest,
    StateMigrationChange, StateMigrationResult, StatePutRequest,
};
use gateway_plugin_sdk::call::model::{CanonicalEvent, ExecutionEvent, WireEvent, WirePayload};
use serde_json::json;

#[test]
fn log_contract_defaults_and_closed_fields_are_stable() {
    let request: LogRequest =
        serde_json::from_value(json!({"event":"provider.connected"})).unwrap();
    assert_eq!(request.level, LogLevel::Info);
    assert!(request.fields.is_empty());
    for level in ["debug", "info", "warn", "error"] {
        let request: LogRequest = serde_json::from_value(
            json!({"event":"provider.connected","level":level,"fields":{"status":200}}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(request).unwrap()["level"], level);
    }
    assert!(
        serde_json::from_value::<LogRequest>(
            json!({"event":"provider.connected","request_id":"forged"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<LogRequest>(json!({"event":"provider.connected","level":"trace"}))
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(LogResult { recorded: false }).unwrap(),
        json!({"recorded":false})
    );
}

#[test]
fn private_state_contract_keeps_cas_and_migration_actions_explicit() {
    let create: StatePutRequest = serde_json::from_value(json!({
        "namespace":"cache", "key":"answer", "value":{"value":42},
        "expected_version":null
    }))
    .unwrap();
    assert!(create.expected_version.is_none());
    assert!(
        serde_json::from_value::<StateDeleteRequest>(json!({
            "namespace":"cache", "key":"answer"
        }))
        .is_err()
    );
    let result: StateMigrationResult = serde_json::from_value(json!({"changes":[
        {"action":"keep","key":"a"},
        {"action":"replace","key":"b","value":{"version":2}},
        {"action":"delete","key":"c"}
    ]}))
    .unwrap();
    assert_eq!(
        result
            .changes
            .iter()
            .map(StateMigrationChange::key)
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
}

#[test]
fn account_callbacks_keep_credentials_in_the_binary_contract_and_require_cas() {
    let list: AuthListRequest = serde_json::from_value(json!({"cursor":null,"limit":20})).unwrap();
    assert_eq!(list.limit, 20);
    assert_eq!(list.provider_id, None);
    let filtered: AuthListRequest =
        serde_json::from_value(json!({"cursor":null,"limit":20,"provider_id":"example"})).unwrap();
    assert_eq!(filtered.provider_id.as_deref(), Some("example"));
    assert!(
        serde_json::from_value::<AuthListRequest>(
            json!({"cursor":null,"limit":20,"grants":["forged"]})
        )
        .is_err()
    );

    let credential: AuthCredential = serde_json::from_value(json!({
        "account_id":"acct_test",
        "provider_id":"example",
        "credential_revision":3,
        "facts":{
            "name":"test",
            "authentication_kind":"api_key",
            "material":{"key":"controlled-test-value"},
            "email":null,
            "upstream_user_id":null,
            "upstream_account_id":null,
            "plan_type":null,
            "access_token_expires_at_ms":null,
            "next_refresh_at_ms":null
        }
    }))
    .unwrap();
    assert_eq!(credential.credential_revision, 3);

    let replace: AuthSaveRequest = serde_json::from_value(json!({
        "action":"replace",
        "account_id":"acct_test",
        "credential_revision":3,
        "facts":{
            "name":"test",
            "authentication_kind":"api_key",
            "material":{"key":"controlled-test-value"},
            "email":null,
            "upstream_user_id":null,
            "upstream_account_id":null,
            "plan_type":null,
            "access_token_expires_at_ms":null,
            "next_refresh_at_ms":null
        }
    }))
    .unwrap();
    assert!(matches!(
        replace,
        AuthSaveRequest::Replace {
            credential_revision: 3,
            ..
        }
    ));
    assert!(
        serde_json::from_value::<AuthSaveRequest>(json!({
            "action":"replace",
            "account_id":"acct_test",
            "facts":{
                "name":"test",
                "authentication_kind":"api_key",
                "material":{"key":"controlled-test-value"},
                "email":null,
                "upstream_user_id":null,
                "upstream_account_id":null,
                "plan_type":null,
                "access_token_expires_at_ms":null,
                "next_refresh_at_ms":null
            }
        }))
        .is_err()
    );
}

#[test]
fn model_and_affinity_callbacks_keep_targets_explicit_and_closed() {
    let request: ModelExecuteRequest = serde_json::from_value(json!({
        "model":"gpt-test",
        "protocol":"openai",
        "operation":"generate",
        "provider":"example",
        "account_id":"acct_test",
        "client_key_id":"key_fixture",
        "previous_response_id":null
    }))
    .unwrap();
    assert_eq!(request.operation, ModelOperation::Generate);
    assert_eq!(request.provider.as_deref(), Some("example"));
    assert_eq!(request.client_key_id.as_deref(), Some("key_fixture"));
    assert!(
        serde_json::from_value::<ModelExecuteRequest>(json!({
            "model":"gpt-test", "protocol":"openai", "operation":"generate",
            "execution_identity":"forged"
        }))
        .is_err()
    );

    let affinity: AffinityLookupRequest =
        serde_json::from_value(json!({"provider":"example","key":"opaque-hash"})).unwrap();
    assert_eq!(affinity.provider, "example");
    assert_eq!(affinity.key, "opaque-hash");
}

#[test]
fn model_event_batch_preserves_binary_wire_and_rejects_trailing_data() {
    let batch = ModelEventBatch {
        events: vec![ExecutionEvent {
            host: None,
            facts: vec![CanonicalEvent::TextDelta {
                index: 0,
                text: "hello".to_owned(),
            }],
            wire: Some(WireEvent {
                protocol: "openai".to_owned(),
                payload: WirePayload::RawJson {
                    body: br#"{"output":"hello"}"#.to_vec(),
                },
            }),
        }],
    };
    let encoded = batch.encode().unwrap();
    let decoded = ModelEventBatch::decode(&encoded).unwrap();
    assert_eq!(decoded.events.len(), 1);
    let wire = decoded.events[0].wire.as_ref().unwrap();
    assert!(matches!(
        &wire.payload,
        WirePayload::RawJson { body } if body == br#"{"output":"hello"}"#
    ));

    let mut trailing = encoded;
    trailing.push(0);
    assert!(ModelEventBatch::decode(&trailing).is_err());
}

#[test]
fn execution_facts_preserve_both_binary_sources_and_do_not_become_output_authority() {
    use gateway_plugin_sdk::call::{middleware::MiddlewareBodyFrame, model::facts::ExecutionFacts};
    let wire = |body| WireEvent {
        protocol: "openai".into(),
        payload: WirePayload::RawBody { body },
    };
    let event = ExecutionEvent {
        facts: vec![],
        wire: Some(wire(vec![0, 255, 1])),
        host: Some(Box::new(ExecutionFacts {
            middleware_origin_wire: Some(wire(vec![128, 0, 3])),
            middleware_transformed: true,
            ..Default::default()
        })),
    };
    let encoded = event.clone().encode().unwrap();
    let decoded = ExecutionEvent::decode(&encoded).unwrap();
    assert!(decoded == event);
    let mut output = MiddlewareBodyFrame::new(b"changed".to_vec(), false);
    output.facts = Some(Box::new(decoded));
    let output = MiddlewareBodyFrame::decode(&output.encode()).unwrap();
    assert_eq!(output.payload, b"changed");
    assert!(
        output.facts.is_none(),
        "snapshots are observations, not a second billing source"
    );
    let mut truncated = encoded.clone();
    truncated.pop();
    assert!(ExecutionEvent::decode(&truncated).is_err());
    let mut wrong_version = encoded;
    wrong_version[3] = b'1';
    assert!(ExecutionEvent::decode(&wrong_version).is_err());
}
