//! Chat 请求映射保留消息、工具和不透明业务数据的语义

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::{
    chat_request, completed,
    http::{Fixture, Session, json_body},
    message,
};

async fn convert(body: Value) -> Value {
    let fixture = Fixture::new(Session::new(vec![completed(json!([message("ok")]))])).await;
    let response = fixture.post(body).await;
    let status = response.status();
    let result = json_body(response).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    fixture.captured().body
}

#[tokio::test]
async fn preserves_ordered_roles_images_and_modern_tool_round_trip() {
    let arguments = "{ \"city\" : \"北京\", \"store\":true }";
    let converted = convert(json!({"model":"model-a","messages":[
        {"role":"system","content":"system text"},
        {"role":"developer","content":"developer text"},
        {"role":"user","content":[{"type":"text","text":"describe"},
            {"type":"image_url","image_url":{"url":"data:image/png;base64,aGVsbG8=","detail":"high"}}]},
        {"role":"assistant","content":"checking","tool_calls":[
            {"id":"call-weather","type":"function","function":{"name":"weather","arguments":arguments}}]},
        {"role":"tool","tool_call_id":"call-weather","content":"{\"temperature\":25}"},
        {"role":"user","content":"continue"}
    ]})).await;
    let input = converted["input"].as_array().unwrap();
    assert_eq!(
        input
            .iter()
            .filter_map(|item| item["role"].as_str())
            .collect::<Vec<_>>(),
        ["system", "developer", "user", "assistant", "user"]
    );
    assert_eq!(
        input[2]["content"][1],
        json!({"type":"input_image","image_url":"data:image/png;base64,aGVsbG8=","detail":"high"})
    );
    assert_eq!(
        input[3]["content"],
        json!([{"type":"output_text","text":"checking","annotations":[]}])
    );
    assert_eq!(
        input[4],
        json!({"type":"function_call","call_id":"call-weather","name":"weather","arguments":arguments})
    );
    assert_eq!(
        input[5],
        json!({"type":"function_call_output","call_id":"call-weather","output":"{\"temperature\":25}"})
    );
    assert!(converted.get("previous_response_id").is_none());
}

#[tokio::test]
async fn assistant_history_uses_output_content_for_text_and_refusals() {
    for content in [
        json!("previous answer"),
        json!([{"type":"text","text":"previous answer"}]),
        json!([{"type":"text","text":"previous answer"},{"type":"refusal","refusal":"declined"}]),
    ] {
        let converted = convert(json!({"model":"model-a","messages":[
            {"role":"user","content":"first turn"},
            {"role":"assistant","content":content},
            {"role":"user","content":"continue from the previous answer"}
        ]}))
        .await;
        let history = &converted["input"][1];
        assert_eq!(history["role"], "assistant");
        assert_eq!(history["status"], "completed");
        assert!(!history["id"].as_str().unwrap().is_empty());
        assert_eq!(
            history["content"][0],
            json!({"type":"output_text","text":"previous answer","annotations":[]})
        );
        if content.is_array() && content.as_array().unwrap().len() == 2 {
            assert_eq!(
                history["content"][1],
                json!({"type":"refusal","refusal":"declined"})
            );
        }
        assert_eq!(converted["input"][2]["content"][0]["type"], "input_text");
    }
}

#[tokio::test]
async fn preserves_nested_schema_metadata_and_future_opaque_values() {
    let schema = json!({"type":"object","properties":{
        "messages":{"type":"string"},"store":{"type":"boolean"},
        "function_call":{"type":"string"},"future":{"x-extension":[1,2]}},
        "required":["messages"],"additionalProperties":true});
    let mut body = chat_request(false);
    body["model"] = json!("future-model-opaque:β");
    body["tools"] = json!([{"type":"function","function":{"name":"inspect","parameters":schema}}]);
    body["tool_choice"] = json!({"type":"function","function":{"name":"inspect"}});
    body["response_format"] = json!({"type":"json_schema","json_schema":{
        "name":"result","description":"preserve","strict":true,"schema":schema}});
    body["metadata"] = json!({"messages":"business data","future":"kept"});
    body["reasoning_effort"] = json!("future-effort");
    body["verbosity"] = json!("future-verbosity");
    body["service_tier"] = json!("future-tier");
    body["future_client_extension"] = json!({"opaque":true});
    let converted = convert(body).await;
    assert_eq!(converted["model"], "future-model-opaque:β");
    assert_eq!(converted["tools"][0]["parameters"], schema);
    assert_eq!(converted["tools"][0]["strict"], false);
    assert_eq!(
        converted["tool_choice"],
        json!({"type":"function","name":"inspect"})
    );
    assert_eq!(converted["text"]["format"]["schema"], schema);
    assert_eq!(converted["text"]["format"]["strict"], true);
    assert_eq!(
        converted["metadata"],
        json!({"messages":"business data","future":"kept"})
    );
    assert_eq!(converted["reasoning"]["effort"], "future-effort");
    assert_eq!(converted["text"]["verbosity"], "future-verbosity");
    assert_eq!(converted["service_tier"], "future-tier");
    assert!(converted.get("future_client_extension").is_none());
}

#[tokio::test]
async fn normalizes_neutral_defaults_without_dropping_shared_provider_parameters() {
    let mut body = chat_request(false);
    for name in [
        "stream",
        "store",
        "n",
        "functions",
        "function_call",
        "logprobs",
        "seed",
    ] {
        body[name] = Value::Null;
    }
    body["frequency_penalty"] = json!(0);
    body["presence_penalty"] = json!(0);
    body["stop"] = json!([]);
    body["max_tokens"] = json!(12);
    body["max_completion_tokens"] = json!(24);
    body["temperature"] = json!(0.2);
    body["top_p"] = json!(0.9);
    body["prompt_cache_retention"] = json!("24h");
    let converted = convert(body).await;
    assert_eq!(converted["stream"], false);
    assert_eq!(converted["store"], false);
    assert_eq!(converted["max_output_tokens"], 24);
    assert_eq!(converted["temperature"], 0.2);
    assert_eq!(converted["top_p"], 0.9);
    assert_eq!(converted["prompt_cache_retention"], "24h");
}

#[tokio::test]
async fn refuses_known_unrepresentable_features_before_starting_execution() {
    for (name, value) in [
        ("n", json!(2)),
        ("store", json!(true)),
        ("stop", json!(["stop"])),
        ("seed", json!(7)),
        ("frequency_penalty", json!(0.3)),
        ("logprobs", json!(true)),
        ("functions", json!([{"name":"old"}])),
        ("function_call", json!("auto")),
        ("input", json!("not chat")),
        ("previous_response_id", json!("resp_previous")),
        ("audio", json!({"voice":"alloy","format":"wav"})),
        (
            "prediction",
            json!({"type":"content","content":"predicted"}),
        ),
    ] {
        let fixture = Fixture::new(Session::new(Vec::new())).await;
        let mut body = chat_request(false);
        body[name] = value;
        let response = fixture.post(body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name}");
        let body = json_body(response).await;
        assert_eq!(body["error"]["param"], name, "{body}");
        assert!(
            fixture.execution.captured.lock().unwrap().is_none(),
            "{name}"
        );
    }
}

#[tokio::test]
async fn rejects_unmapped_message_content_and_identity_fields() {
    for message in [
        json!({"role":"user","name":"speaker","content":"hello"}),
        json!({"role":"user","content":[{"type":"file","file":{"file_id":"file-example"}}]}),
        json!({"role":"assistant","content":"hello","reasoning_content":"private"}),
        json!({"role":"assistant","content":[{"type":"text","text":"history","prompt_cache_breakpoint":{"type":"default"}}]}),
        json!({"role":"tool","content":"unassociated output"}),
    ] {
        let fixture = Fixture::new(Session::new(Vec::new())).await;
        let response = fixture
            .post(json!({"model":"model-a","messages":[message]}))
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = json_body(response).await;
        assert!(
            body["error"]["param"]
                .as_str()
                .unwrap()
                .starts_with("messages")
        );
        assert!(fixture.execution.captured.lock().unwrap().is_none());
    }
}

#[tokio::test]
async fn assistant_tool_history_keeps_arguments_opaque_and_cache_annotations_structural() {
    let cache = json!({"type":"default","future_setting":{"keep":true}});
    let arguments = "not valid JSON {\n  \"raw\" : 🦀";
    let converted = convert(json!({"model":"model-a","messages":[
        {"role":"user","content":[{"type":"text","text":"cached","prompt_cache_breakpoint":cache}]},
        {"role":"assistant","content":null,"tool_calls":[{"id":"call-raw","type":"function",
            "function":{"name":"raw","arguments":arguments}}]},
        {"role":"tool","tool_call_id":"call-raw","content":[{"type":"text","text":"result"}]}
    ],"prompt_cache_options":{"future":{"keep":true}},"moderation":{"future":{"keep":true}}}))
    .await;
    assert_eq!(
        converted["input"][0]["content"][0]["prompt_cache_breakpoint"],
        cache
    );
    assert_eq!(converted["input"][1]["arguments"], arguments);
    assert_eq!(
        converted["input"][2]["output"],
        json!([{"type":"input_text","text":"result"}])
    );
    assert_eq!(
        converted["prompt_cache_options"],
        json!({"future":{"keep":true}})
    );
    assert_eq!(converted["moderation"], json!({"future":{"keep":true}}));
}
