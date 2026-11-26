//! Grok 原生请求的字段保留、账号隔离与模型绑定测试

use gateway_core::operation::{GenerateRequest, ProtocolPayload};
use gateway_core::policy::ClientApiKeyId;
use serde_json::{Value, json};

use provider_xai::GrokResponsesRequest;

mod model_identity;

fn raw_request(body: Value) -> GenerateRequest {
    let Value::Object(body) = body else {
        panic!("request fixture must be an object");
    };
    GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body).expect("OpenAI payload"),
    )
}

fn client_key() -> ClientApiKeyId {
    ClientApiKeyId::new("key_xai_request_test").expect("client key id")
}

#[test]
fn account_identity_should_be_removed_without_touching_prompt_content() {
    let request = raw_request(json!({
        "model": "client-model",
        "input": [{
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "account_id and x-userid are ordinary prompt text"
            }]
        }],
        "authorization": "Bearer attacker",
        "account_id": "attacker-account",
        "user_id": "attacker-user",
        "team_id": "attacker-team",
        "conversation_id": "attacker-conversation",
        "previous_response_id": "attacker-response",
        "metadata": {
            "accountId": "nested-attacker-account",
            "session_id": "nested-attacker-session",
            "application_tag": "preserve-me"
        }
    }));

    let encoded = GrokResponsesRequest::encode(&request, "grok-routed", &client_key())
        .expect("sanitized request");
    let body = Value::Object(encoded.body().clone());

    for pointer in [
        "/authorization",
        "/account_id",
        "/user_id",
        "/team_id",
        "/conversation_id",
        "/previous_response_id",
    ] {
        assert_eq!(
            body.pointer(pointer),
            None,
            "identity survived at {pointer}"
        );
    }
    assert_eq!(
        body.pointer("/input/0/content/0/text"),
        Some(&json!("account_id and x-userid are ordinary prompt text"))
    );
    assert_eq!(
        body["metadata"],
        json!({"accountId":"nested-attacker-account","session_id":"nested-attacker-session","application_tag":"preserve-me"})
    );
}

#[test]
fn request_debug_should_not_expose_prompt_or_unknown_values() {
    let request = raw_request(json!({
        "model": "client-model",
        "input": "private prompt",
        "future_secret_shaped_value": "must-not-leak"
    }));
    let encoded =
        GrokResponsesRequest::encode(&request, "grok-routed", &client_key()).expect("raw request");
    let debug = format!("{encoded:?}");

    assert!(!debug.contains("private prompt"));
    assert!(!debug.contains("must-not-leak"));
}

#[test]
fn explicit_session_should_be_tenant_isolated_and_stable_across_turns() {
    let first = raw_request(json!({
        "model": "client",
        "prompt_cache_key": "conversation-42",
        "input": [{"role": "user", "content": "first"}]
    }));
    let second = raw_request(json!({
        "model": "client",
        "prompt_cache_key": "conversation-42",
        "input": [
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": "answer"},
            {"role": "user", "content": "second"}
        ]
    }));
    let key_a = client_key();
    let key_b = ClientApiKeyId::new("key_xai_other_tenant").expect("client key id");

    let first_a = GrokResponsesRequest::encode(&first, "grok-4.5", &key_a).expect("first");
    let second_a = GrokResponsesRequest::encode(&second, "grok-4.5", &key_a).expect("second");
    let first_b = GrokResponsesRequest::encode(&first, "grok-4.5", &key_b).expect("other");

    assert_eq!(first_a.session_id(), second_a.session_id());
    assert_eq!(first_a.affinity(), second_a.affinity());
    assert_ne!(first_a.session_id(), first_b.session_id());
    assert_eq!(
        first_a
            .body()
            .get("prompt_cache_key")
            .and_then(Value::as_str),
        first_a.session_id()
    );
}

#[test]
fn soft_session_should_follow_the_first_user_anchor() {
    let first = raw_request(json!({
        "model": "client",
        "instructions": "stable system",
        "input": [{"type": "message", "role": "user", "content": "first prompt"}]
    }));
    let later = raw_request(json!({
        "model": "client",
        "instructions": "stable system",
        "input": [
            {"type": "message", "role": "user", "content": "first prompt"},
            {"type": "message", "role": "assistant", "content": "answer"},
            {"type": "message", "role": "user", "content": "next prompt"}
        ]
    }));

    let first = GrokResponsesRequest::encode(&first, "grok-4.5", &client_key()).expect("first");
    let later = GrokResponsesRequest::encode(&later, "grok-4.5", &client_key()).expect("later");

    assert_eq!(first.session_id(), later.session_id());
    assert_eq!(first.affinity(), later.affinity());
}

#[test]
fn tool_schema_properties_survive_protocol_field_sanitization() {
    let schema = json!({"type":"object","properties":{"external_web_access":{"type":"boolean"},"q":{"type":"string","external_web_access":true}}});
    let encoded = GrokResponsesRequest::encode(
        &raw_request(json!({
            "input":"hello", "tools":[{"type":"function","name":"lookup","parameters":schema}]
        })),
        "grok-4.6",
        &client_key(),
    )
    .unwrap();
    assert_eq!(encoded.body()["tools"][0]["parameters"], schema);
}

#[test]
fn native_business_fields_and_harness_items_are_not_rewritten() {
    let fields = json!({
        "input": [
            {"type":"message","role":"assistant","phase":"analysis","content":[{"type":"output_text","text":"hello"}],"internal_chat_message_metadata_passthrough":{"native":true}},
            {"type":"compaction","encrypted_content":"opaque","summary":"summary"},
            {"type":"compaction_trigger"},
            {"type":"custom_tool_call","call_id":"c1","name":"apply_patch","input":"patch"}
        ],
        "tools":[{"type":"namespace","name":"tools","tools":[{"type":"custom","name":"apply_patch","format":{"type":"grammar","definition":"raw"}}]},{"type":"tool_search"}],
        "tool_choice":{"type":"custom","namespace":"tools","name":"apply_patch"},
        "reasoning":{"effort":"  HIGH  ","native_flag":true},
        "include":["native.future"],
        "service_tier":"priority",
        "prompt_cache_retention":"24h",
        "safety_identifier":"caller",
        "external_web_access":false,
        "metadata":{"application":"client","user_id":"user_session_other"},
        "client_metadata":{"native_context":"unchanged"},
        "future_extension":{"tool_name":"apply_patch","arguments":{"n":1.5}}
    });
    let encoded =
        GrokResponsesRequest::encode(&raw_request(fields.clone()), "grok-4.7", &client_key())
            .unwrap();
    for (key, value) in fields.as_object().unwrap() {
        assert_eq!(encoded.body().get(key), Some(value), "{key}");
    }
    assert!(!encoded.body().contains_key("store"));
}
