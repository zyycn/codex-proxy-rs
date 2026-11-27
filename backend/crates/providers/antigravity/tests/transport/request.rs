//! 系统指令合成、身份字段局部修改与其余 JSON 原始字节保留

use crate::{profile, transport::CloudCodeClient};
use serde_json::json;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

#[tokio::test]
async fn oauth_identity_edits_preserve_remaining_json_bytes_and_duplicate_business_fields() {
    let server = MockServer::start().await;
    Mock::given(path("/v1internal:generateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"response":{}})))
        .mount(&server)
        .await;
    let selection = profile::Selection {
        system_prompt: Some("OAuth fixture".into()),
        ..Default::default()
    };
    let client = CloudCodeClient::new(
        &server.uri().parse().unwrap(),
        None,
        &selection.resolve("2.34.0"),
    )
    .unwrap();
    let instruction = r#""systemInstruction":{"parts":[{"text":"OAuth fixture"}]}"#;
    for (body, remaining) in [
        (
            r#"{ "contents" : [], "unknown":98765432109876543210987654, "unknown":1e+09, "tools" : [ { "functionDeclarations":[] } ] }"#,
            r#"{ "contents" : [], "unknown":98765432109876543210987654, "unknown":1e+09, "tools" : [ { "functionDeclarations":[] } ]"#,
        ),
        (
            r#"{"system_instruction":{}, "system\u0049nstruction":{},"contents":[],"systemInstruction":{}}"#,
            r#"{"contents":[]"#,
        ),
        (
            r#"{"labels":{"client":"gateway", "cascade_id" : "conversation", "goog-originating-service-id":"key-mode","goog-originating-logical-product-id":"key-mode","client":"again","custom" : 1e+09},"sessionId":"unchanged","contents":[{"parts":[{"thoughtSignature":"opaque","functionResponse":{"name":"tool","response":{"text":"auth_method=gateway"}}}]}]}"#,
            r#"{"labels":{ "cascade_id" : "conversation","custom" : 1e+09},"sessionId":"unchanged","contents":[{"parts":[{"thoughtSignature":"opaque","functionResponse":{"name":"tool","response":{"text":"auth_method=gateway"}}}]}]"#,
        ),
        (
            r#"{"labels":{"client":"gateway"},"contents":[]}"#,
            r#"{"labels":{},"contents":[]"#,
        ),
        (r#"{}"#, "{"),
        (r#"{"system_instruction":{}}"#, "{"),
    ] {
        client
            .generate(
                "account-project",
                "fixture-model",
                "fixture-access",
                body.as_bytes(),
                false.into(),
                &[],
            )
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let envelope: std::collections::BTreeMap<String, &serde_json::value::RawValue> =
            serde_json::from_slice(&requests.last().unwrap().body).unwrap();
        let suffix = if body.ends_with(" }") { " }" } else { "}" };
        let comma = if remaining == "{" { "" } else { "," };
        assert_eq!(
            envelope["request"].get(),
            format!("{remaining}{comma}{instruction}{suffix}")
        );
        assert_eq!(envelope["project"].get(), "\"account-project\"");
        assert_eq!(envelope["userAgent"].get(), "\"antigravity\"");
        assert_eq!(envelope["requestType"].get(), "\"agent\"");
    }
    for invalid in ["[]", "null", "{", "{} trailing"] {
        assert!(
            client
                .generate(
                    "account-project",
                    "fixture-model",
                    "fixture-access",
                    invalid.as_bytes(),
                    false.into(),
                    &[]
                )
                .await
                .is_err()
        );
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 6);
}

#[tokio::test]
async fn generation_and_count_preserve_client_rules_and_instruction_metadata() {
    let server = MockServer::start().await;
    Mock::given(path("/v1internal:generateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"response":{}})))
        .mount(&server)
        .await;
    Mock::given(path("/v1internal:countTokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"totalTokens":42})))
        .mount(&server)
        .await;
    let selection = profile::Selection {
        system_prompt: Some("Configured prompt".into()),
        ..Default::default()
    };
    let client = CloudCodeClient::new(
        &server.uri().parse().unwrap(),
        None,
        &selection.resolve("2.34.0"),
    )
    .unwrap();
    // CLI 1.5.18 Gateway 请求把全局和项目规则放在系统指令中
    let parts = r#"{"text":"Client tool instructions"},{ "text" : "<user_rules><user_global>Global rule</user_global><workspace>Project rule</workspace></user_rules>", "extension":1e+09 }"#;
    let instruction =
        format!(r#"{{"role":"system", "extension":98765432109876543210987654,"parts":[{parts}]}}"#);
    for field in [
        "systemInstruction",
        "system_instruction",
        "system\\u0049nstruction",
    ] {
        let body = format!(r#"{{"{field}":{instruction},"contents":[]}}"#);
        client
            .generate(
                "project",
                "model",
                "token",
                body.as_bytes(),
                false.into(),
                &[],
            )
            .await
            .unwrap();
        client
            .count_tokens("token", body.as_bytes(), &[])
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        for request in requests.iter().rev().take(2) {
            let body: std::collections::BTreeMap<String, &serde_json::value::RawValue> =
                serde_json::from_slice(&request.body).unwrap();
            let body: std::collections::BTreeMap<String, &serde_json::value::RawValue> =
                serde_json::from_str(body["request"].get()).unwrap();
            assert_eq!(
                body["systemInstruction"].get(),
                format!(
                    r#"{{"role":"system", "extension":98765432109876543210987654,"parts":[{{"text":"Configured prompt"}},{parts}]}}"#
                )
            );
            assert!(!body.contains_key("system_instruction"));
        }
    }
}
