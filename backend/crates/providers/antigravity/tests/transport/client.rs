//! 通过回环 HTTP 验证授权凭据、原生请求与响应透传

use crate::{
    credential::refreshed_credential,
    profile::{self, ReleaseService},
    transport::{self, CloudCodeClient},
};
use futures::{StreamExt, TryStreamExt};
use gateway_core::{
    event::{EventSequenceValidator, GatewayEvent},
    lifecycle::{CancellationToken, Deadline},
    routing::UpstreamModelId,
};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

#[tokio::test]
async fn oauth_refresh_and_native_generate_preserve_grant_profile_and_wire_payload() {
    let server = MockServer::start().await;
    let (origin, access_token) = refreshed_credential(&server).await;
    let profile =
        profile::default_profile(&ReleaseService::new(crate::support::profile::cache()).unwrap());
    // 官方 macOS arm64 Desktop 2.34.0 在默认参数下实际发送的已认证请求头
    let user_agent =
        "antigravity/hub/2.34.0 (aidev_client; os_type=darwin; arch=arm64; cl=1026637980)";
    let client = CloudCodeClient::new(&origin, None, &profile).unwrap();
    let post = |p| {
        Mock::given(method("POST"))
            .and(path(p))
            .and(header("authorization", "Bearer renewed-access"))
            .and(header("user-agent", user_agent))
            .and(header("accept", "*/*"))
            .and(header("accept-encoding", "gzip"))
    };
    post("/v1internal:loadCodeAssist")
        .respond_with(
            compressed_response(
                200,
                br#"{"cloudaicompanionProject":"project-1"}"#,
                "application/json",
            )
            .await,
        )
        .expect(1)
        .mount(&server)
        .await;
    post("/v1internal:fetchAvailableModels")
        .and(wiremock::matchers::body_json(
            json!({"project":"project-1"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"models":{"fixture-model":{"displayName":"Fixture Model"}}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let catalog = client.catalog(&access_token).await.unwrap();
    assert_eq!(catalog.models["fixture-model"].name(), "Fixture Model");
    let body=br#"{ "contents":[{"parts":[{"functionResponse":{"name":"lookup","response":{"x":1}},"thoughtSignature":"opaque"}]}],"unknown":98765432109876543210987654,"systemInstruction":{"parts":[{"text":"You are Claude Code."}]},"system_instruction":{"parts":[{"text":"old"}]} }"#;
    let reply = r#"{ "responseId":"r","candidates":[{"content":{"parts":[{"text":"hidden","thought":true},{"text":"ok"},{"functionCall":{"name":"lookup","args":{"text":"not output"}}}]},"finishReason":"STOP"},{"index":1,"content":{"parts":[{"text":"other candidate"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":2},"extra":98765432109876543210987654 }"#;
    let received_body = move |r: &wiremock::Request| {
        let envelope: std::collections::BTreeMap<String, &serde_json::value::RawValue> =
            serde_json::from_slice(&r.body).unwrap();
        let request: std::collections::BTreeMap<String, &serde_json::value::RawValue> =
            serde_json::from_str(envelope["request"].get()).unwrap();
        let original: std::collections::BTreeMap<String, &serde_json::value::RawValue> =
            serde_json::from_slice(body).unwrap();
        let system: serde_json::Value =
            serde_json::from_str(request["systemInstruction"].get()).unwrap();
        request["contents"].get() == original["contents"].get()
            && request["unknown"].get() == original["unknown"].get()
            && !request.contains_key("system_instruction")
            && system["parts"][0]["text"]
                .as_str()
                .unwrap()
                .starts_with("You are Antigravity,")
            && system["parts"][1]["text"] == "You are Claude Code."
            && system["parts"].as_array().unwrap().len() == 2
    };
    post("/v1internal:generateContent")
        .and(received_body)
        .respond_with(
            compressed_response(
                200,
                format!(r#"{{"response":{reply}}}"#).as_bytes(),
                "application/json",
            )
            .await,
        )
        .expect(1)
        .mount(&server)
        .await;
    post("/v1internal:streamGenerateContent")
        .and(query_param("alt", "sse"))
        .and(received_body)
        .respond_with(
            compressed_response(
                200,
                format!("id: evt-1\ndata: {{\"response\":{reply}}}\n\n").as_bytes(),
                "text/event-stream",
            )
            .await,
        )
        .expect(1)
        .mount(&server)
        .await;
    for stream in [false, true] {
        let res = client
            .generate(
                &catalog.project,
                "fixture-model",
                &access_token,
                body,
                stream.into(),
                &[],
            )
            .await
            .unwrap();
        let events = transport::events(
            res,
            transport::ResponseContext {
                stream,
                model: UpstreamModelId::new("fixture-model").unwrap(),
                deadline: Deadline::default(),
                cancel: CancellationToken::new(),
                token: access_token.clone(),
                pricing: None,
                started_at: std::time::Instant::now(),
            },
        )
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
        let facts: Vec<_> = events
            .iter()
            .flat_map(|event| event.canonical_facts())
            .collect();
        let mut validator = EventSequenceValidator::default();
        for fact in &facts {
            validator.observe(fact).unwrap();
        }
        assert!(matches!(facts.first(), Some(GatewayEvent::Started(_))));
        assert!(facts.iter().any(|fact| matches!(fact, GatewayEvent::Usage(u) if u.input_tokens == Some(4) && u.output_tokens == Some(2))));
        let text: String = facts
            .iter()
            .filter_map(|fact| match fact {
                GatewayEvent::TextDelta(delta) => Some(delta.text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "ok");
        assert!(matches!(
            events.last().unwrap().canonical_facts(),
            [GatewayEvent::Completed(_)]
        ));
        let wire = events[0].wire_event().unwrap();
        if stream {
            assert!(
                std::str::from_utf8(wire.raw_sse_frame().unwrap())
                    .unwrap()
                    .contains(&format!("id: evt-1\ndata: {reply}\n\n"))
            );
        } else {
            assert_eq!(wire.raw_json_body().unwrap().as_ref(), reply.as_bytes());
        }
    }
}

#[tokio::test]
async fn streamed_text_is_projected_once_across_native_frames() {
    let server = MockServer::start().await;
    let profile =
        profile::default_profile(&ReleaseService::new(crate::support::profile::cache()).unwrap());
    let client = CloudCodeClient::new(&server.uri().parse().unwrap(), None, &profile).unwrap();
    let frames = [
        json!({"candidates":[{"content":{"parts":[{"text":""}]}}]}),
        json!({"responseId":"r","candidates":[{"content":{"parts":[{"text":"hidden","thought":true}]}}]}),
        json!({"candidates":[{"index":1,"content":{"parts":[{"text":"other"}]},"finishReason":"STOP"},{"index":0,"content":{"parts":[{"text":"O"}]}}]}),
        json!({"candidates":[{"index":0,"content":{"parts":[{"text":"K"}]},"finishReason":"STOP"}]}),
        json!({"usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":2}}),
    ];
    let body: String = frames
        .iter()
        .map(|frame| format!("data: {{\"response\":{frame}}}\n\n"))
        .collect();
    Mock::given(path("/v1internal:streamGenerateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;
    let response = client
        .generate("p", "m", "fixture", b"{}", true.into(), &[])
        .await
        .unwrap();
    let events = transport::events(
        response,
        transport::ResponseContext {
            stream: true,
            model: UpstreamModelId::new("m").unwrap(),
            deadline: Deadline::default(),
            cancel: CancellationToken::new(),
            token: "fixture".into(),
            pricing: None,
            started_at: std::time::Instant::now(),
        },
    )
    .try_collect::<Vec<_>>()
    .await
    .unwrap();
    let mut validator = EventSequenceValidator::default();
    let mut text = String::new();
    let mut content_count = 0;
    for fact in events.iter().flat_map(|event| event.canonical_facts()) {
        validator.observe(fact).unwrap();
        match fact {
            GatewayEvent::ContentAdded(_) => content_count += 1,
            GatewayEvent::TextDelta(delta) => text.push_str(&delta.text),
            _ => {}
        }
    }
    assert_eq!(content_count, 1);
    assert_eq!(text, "OK");
    let timings: Vec<_> = events
        .iter()
        .filter_map(|event| event.response_observation())
        .map(|observation| observation.timings())
        .collect();
    assert!(timings[0].headers_ms.is_some());
    assert!(timings[0].first_event_ms.is_some());
    assert!(timings[0].first_token_ms.is_none());
    assert!(timings[1].first_reasoning_ms.is_some());
    assert!(timings[1].first_text_ms.is_none());
    assert!(timings[2].first_text_ms.is_some());
    let wire: Vec<_> = events
        .iter()
        .filter_map(|event| event.wire_event())
        .collect();
    assert_eq!(wire.len(), frames.len());
    for (wire, frame) in wire.iter().zip(frames) {
        assert_eq!(
            wire.raw_sse_frame().unwrap().as_ref(),
            format!("data: {frame}\n\n").as_bytes()
        );
    }
}

#[tokio::test]
async fn incomplete_native_stream_and_redirect_are_rejected() {
    let server = MockServer::start().await;
    let profile =
        profile::default_profile(&ReleaseService::new(crate::support::profile::cache()).unwrap());
    let client = CloudCodeClient::new(&server.uri().parse().unwrap(), None, &profile).unwrap();
    Mock::given(path("/v1internal:streamGenerateContent")).respond_with(ResponseTemplate::new(200).set_body_raw("data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}]}}]}}\n\n","text/event-stream")).mount(&server).await;
    let res = client
        .generate("p", "m", "fixture", b"{}", true.into(), &[])
        .await
        .unwrap();
    let result = transport::events(
        res,
        transport::ResponseContext {
            stream: true,
            model: UpstreamModelId::new("m").unwrap(),
            deadline: Deadline::default(),
            cancel: CancellationToken::new(),
            token: "fixture".into(),
            pricing: None,
            started_at: std::time::Instant::now(),
        },
    )
    .try_collect::<Vec<_>>()
    .await;
    assert_eq!(
        result.unwrap_err().kind(),
        gateway_core::error::ProviderErrorKind::Protocol
    );
    let other = MockServer::start().await;
    Mock::given(path("/v1internal:generateContent"))
        .respond_with(ResponseTemplate::new(307).insert_header("Location", other.uri()))
        .mount(&server)
        .await;
    assert!(
        client
            .generate("p", "m", "fixture", b"{}", false.into(), &[])
            .await
            .is_err()
    );
    assert!(other.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn incomplete_stream_never_emits_a_calculated_cost() {
    let server = MockServer::start().await;
    let frame = json!({"response":{
        "candidates":[{"content":{"parts":[{"text":"partial"}]}}],
        "usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":20}
    }});
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(format!("data: {frame}\n\n"), "text/event-stream"),
        )
        .mount(&server)
        .await;
    let response = reqwest::get(server.uri()).await.unwrap();
    let events = transport::events(
        response,
        transport::ResponseContext {
            stream: true,
            model: UpstreamModelId::new("fixture-model").unwrap(),
            deadline: Deadline::default(),
            cancel: CancellationToken::new(),
            token: "fixture".into(),
            pricing: Some(crate::billing_tests::pricing()),
            started_at: std::time::Instant::now(),
        },
    )
    .collect::<Vec<_>>()
    .await;
    assert_eq!(
        events.last().unwrap().as_ref().unwrap_err().kind(),
        gateway_core::error::ProviderErrorKind::Protocol
    );
    assert!(
        !events
            .iter()
            .filter_map(|event| event.as_ref().ok())
            .flat_map(|event| event.canonical_facts())
            .any(|fact| matches!(fact, GatewayEvent::CalculatedCost(_)))
    );
}

async fn compressed_response(status: u16, body: &[u8], media: &str) -> ResponseTemplate {
    use tokio::io::AsyncWriteExt;
    let mut compressed = Vec::new();
    // 覆盖多个 gzip member，后半段包含协议完成与用量，不能被解码器提前截断
    for part in body.chunks(body.len().div_ceil(2)) {
        let mut encoder = async_compression::tokio::write::GzipEncoder::new(Vec::new());
        encoder.write_all(part).await.unwrap();
        encoder.shutdown().await.unwrap();
        compressed.extend(encoder.into_inner());
    }
    ResponseTemplate::new(status)
        .insert_header("content-encoding", "gzip")
        .set_body_raw(compressed, media)
}

#[tokio::test]
async fn compressed_control_responses_keep_errors_and_decoded_size_limits() {
    use gateway_core::error::ProviderErrorKind;
    let server = MockServer::start().await;
    let profile =
        profile::default_profile(&ReleaseService::new(crate::support::profile::cache()).unwrap());
    let client = CloudCodeClient::new(&server.uri().parse().unwrap(), None, &profile).unwrap();
    Mock::given(path("/v1internal:loadCodeAssist"))
        .respond_with(
            compressed_response(
                429,
                br#"{"error":{"message":"fixture quota"}}"#,
                "application/json",
            )
            .await,
        )
        .mount(&server)
        .await;
    let error = client.catalog("fixture").await.err().unwrap();
    assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
    assert_eq!(error.upstream_status(), Some(429));
    server.reset().await;
    Mock::given(path("/v1internal:loadCodeAssist"))
        .respond_with(
            compressed_response(200, &vec![b' '; 8 * 1024 * 1024 + 1], "application/json").await,
        )
        .mount(&server)
        .await;
    let error = client.catalog("fixture").await.err().unwrap();
    assert_eq!(error.kind(), ProviderErrorKind::MessageTooBig);
    server.reset().await;
    Mock::given(path("/v1internal:loadCodeAssist"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-encoding", "gzip")
                .set_body_raw("invalid gzip", "application/json"),
        )
        .mount(&server)
        .await;
    let error = client.catalog("fixture").await.err().unwrap();
    assert_eq!(error.kind(), ProviderErrorKind::Transport);
}
