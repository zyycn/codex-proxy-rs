//! 实际上游请求保留业务头的多值字节，账号和传输字段由 AGY 重新构造

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures::TryStreamExt;
use gateway_core::{
    engine::provider::{Provider, ProviderRequest},
    operation::{Operation, RawJsonPayload, TokenCountRequest},
};
use serde_json::json;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

use crate::support::{Fixture, context, request};

#[tokio::test]
async fn native_and_translated_requests_forward_business_headers_with_account_identity() {
    let server = MockServer::start().await;
    let fixture = Fixture::new(&server.uri());
    let profile = crate::profile::default_profile(
        &crate::profile::ReleaseService::new(crate::support::profile::cache()).unwrap(),
    )
    .frozen();
    let ctx = || {
        context(
            profile.clone(),
            gateway_core::lifecycle::CancellationToken::new(),
        )
    };
    let reply = json!({"response":{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP"}]}});
    for (route, body, media) in [
        ("generateContent", reply.to_string(), "application/json"),
        (
            "streamGenerateContent",
            format!("data: {reply}\n\n"),
            "text/event-stream",
        ),
        (
            "countTokens",
            json!({"totalTokens":2}).to_string(),
            "application/json",
        ),
    ] {
        Mock::given(path(format!("/v1internal:{route}")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, media))
            .mount(&server)
            .await;
    }
    let forbidden = [
        "authorization",
        "cookie",
        "cookie2",
        "user-agent",
        "x-goog-api-key",
        "x-goog-user-project",
        "x-goog-api-client",
        "content-encoding",
        "content-length",
        "x-request-id",
        "chatgpt-account-id",
        "x-oai-attestation",
    ];
    let mut headers: Vec<_> = forbidden
        .iter()
        .map(|name| json!([name, STANDARD.encode("forbidden")]))
        .collect();
    headers.extend([
        json!(["x-business-extension", STANDARD.encode(b"first\x80")]),
        json!(["x-business-extension", STANDARD.encode("second")]),
        json!(["traceparent", STANDARD.encode("original-trace")]),
    ]);
    let metadata = json!({"opaque_request_headers":headers});
    for (protocol, body) in [
        (
            gateway_protocol::genai::GENAI_PROTOCOL,
            json!({"contents":[]}),
        ),
        (
            gateway_protocol::openai::OPENAI_PROTOCOL,
            json!({"input":[]}),
        ),
        (
            gateway_protocol::anthropic::ANTHROPIC_PROTOCOL,
            json!({"messages":[]}),
        ),
    ] {
        let input = super::affinity::session_request(protocol, body, metadata.clone());
        fixture
            .provider
            .clone()
            .execute(input, ctx())
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
    }
    let input = ProviderRequest::new(
        Operation::CountTokens(TokenCountRequest::from_raw_json(
            RawJsonPayload::new(
                gateway_protocol::genai::GENAI_PROTOCOL,
                br#"{"contents":[]}"#.as_slice().into(),
            )
            .unwrap()
            .with_context(metadata.as_object().unwrap().clone()),
        )),
        request(false).candidate().clone(),
    );
    fixture
        .provider
        .clone()
        .execute(input, ctx())
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);
    for request in requests {
        let values: Vec<_> = request
            .headers
            .get_all("x-business-extension")
            .iter()
            .map(|value| value.as_bytes())
            .collect();
        assert_eq!(values, vec![b"first\x80".as_slice(), b"second"]);
        assert_eq!(request.headers["traceparent"], "original-trace");
        assert_eq!(request.headers["authorization"], "Bearer fixture-access");
        assert_eq!(request.headers["content-type"], "application/json");
        assert!(
            request.headers["user-agent"]
                .to_str()
                .unwrap()
                .starts_with("antigravity/hub/")
        );
        for name in forbidden {
            if !matches!(name, "authorization" | "user-agent" | "content-length") {
                assert!(!request.headers.contains_key(name), "unexpected {name}");
            }
        }
    }
}
