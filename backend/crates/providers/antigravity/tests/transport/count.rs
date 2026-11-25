//! 计数使用独立上游、共享身份策略并保留错误与资源生命周期

use crate::{
    profile::{Selection, object},
    support::{Fixture, context, request},
};
use futures::{StreamExt, TryStreamExt};
use gateway_core::{
    account::OpaqueProviderData,
    engine::provider::Provider,
    error::ProviderErrorKind,
    event::GatewayEvent,
    lifecycle::CancellationToken,
    operation::{Operation, RawJsonPayload, TokenCountRequest},
    upstream::UpstreamSendState,
};
use serde_json::{Value, json};
use std::time::Duration;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

fn profile() -> OpaqueProviderData {
    Selection::parse(&object(json!({"client":"cli","platform":"linux","versionMode":"fixed","clientVersion":"1.5.14","arch":null,"systemPrompt":"count identity"}))).unwrap().resolve("9.9.9").frozen()
}

fn counted(protocol: &str, body: Value) -> gateway_core::engine::provider::ProviderRequest {
    let request = request(false);
    let operation = Operation::CountTokens(TokenCountRequest::from_raw_json(
        RawJsonPayload::new(protocol, serde_json::to_vec(&body).unwrap().into()).unwrap(),
    ));
    gateway_core::engine::provider::ProviderRequest::new(operation, request.candidate().clone())
}

async fn released(fixture: &Fixture) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while fixture.store.active_leases() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn native_and_messages_count_share_identity_and_never_generate_usage() {
    for (protocol, body, expected) in [
        (
            "genai",
            json!({"generateContentRequest":{"model":"models/ignored","contents":[{"role":"user","parts":[{"text":"hello"}]}],"systemInstruction":{"parts":[{"text":"client identity"}]},"tools":[{"functionDeclarations":[{"name":"read"}]}],"toolConfig":{},"safetySettings":[]}}),
            json!({"totalTokens":42}),
        ),
        (
            "anthropic",
            json!({"model":"fixture-model","messages":[{"role":"user","content":"hello"}],"system":"client identity","tools":[{"name":"read","input_schema":{"type":"object"}}]}),
            json!({"input_tokens":42}),
        ),
    ] {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:countTokens"))
            .and(header("authorization", "Bearer fixture-access"))
            .and(header("user-agent", "antigravity/cli/1.5.14 (aidev_client; os_type=linux; arch=amd64; auth_method=consumer)"))
            .and(header("accept-encoding", "gzip"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"totalTokens":42})))
            .expect(1)
            .mount(&upstream)
            .await;
        let fixture = Fixture::new(&upstream.uri());
        let stream = fixture
            .provider
            .clone()
            .execute(
                counted(protocol, body),
                context(profile(), CancellationToken::new()),
            )
            .await
            .unwrap();
        assert!(upstream.received_requests().await.unwrap().is_empty());
        let mut stream = stream;
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            let event = event.unwrap();
            let terminal = event
                .canonical_facts()
                .iter()
                .any(|fact| matches!(fact, GatewayEvent::Completed(_)));
            events.extend(stream.translate_native_response(event, terminal).unwrap());
        }
        drop(stream);
        assert!(events.iter().any(|event| {
            event.wire_event().is_some_and(|wire| {
                wire.protocol() == protocol
                    && wire.raw_json_body().map_or_else(
                        || wire.data().clone(),
                        |raw| serde_json::from_slice::<Value>(raw).unwrap(),
                    ) == expected
            })
        }));
        assert!(
            !events
                .iter()
                .flat_map(|event| event.canonical_facts())
                .any(|fact| matches!(
                    fact,
                    GatewayEvent::Usage(_) | GatewayEvent::CalculatedCost(_)
                ))
        );
        let sent = upstream.received_requests().await.unwrap();
        let sent: Value = sent[0].body_json().unwrap();
        assert_eq!(sent.as_object().unwrap().len(), 1);
        assert_eq!(
            sent["request"]["systemInstruction"]["parts"][0]["text"],
            "count identity"
        );
        assert_eq!(sent["request"]["contents"][0]["parts"][0]["text"], "hello");
        assert_eq!(
            sent["request"]["tools"][0]["functionDeclarations"][0]["name"],
            "read"
        );
        assert!(sent["request"].get("model").is_none());
        assert!(sent["request"].get("toolConfig").is_none());
        assert!(sent["request"].get("safetySettings").is_none());
        released(&fixture).await;
    }
}

#[tokio::test]
async fn count_cancellation_before_poll_does_not_send_or_leak() {
    let upstream = MockServer::start().await;
    let fixture = Fixture::new(&upstream.uri());
    let cancellation = CancellationToken::new();
    let mut stream = fixture
        .provider
        .clone()
        .execute(
            counted("genai", json!({"contents":[]})),
            context(profile(), cancellation.clone()),
        )
        .await
        .unwrap();
    cancellation.cancel();
    let err = stream.next().await.unwrap().unwrap_err();
    assert_eq!(err.kind(), ProviderErrorKind::Cancelled);
    assert_eq!(err.send_state(), UpstreamSendState::NotSent);
    drop(stream);
    released(&fixture).await;
    assert!(upstream.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn upstream_count_failure_has_no_local_estimate() {
    for (status, body, kind) in [
        (
            400,
            json!({"error":{"message":"invalid contents"}}),
            ProviderErrorKind::InvalidRequest,
        ),
        (200, json!({"totalTokens":-1}), ProviderErrorKind::Protocol),
    ] {
        let upstream = MockServer::start().await;
        Mock::given(path("/v1internal:countTokens"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .expect(1)
            .mount(&upstream)
            .await;
        let fixture = Fixture::new(&upstream.uri());
        let stream = fixture
            .provider
            .clone()
            .execute(
                counted("genai", json!({"contents":[]})),
                context(profile(), CancellationToken::new()),
            )
            .await
            .unwrap();
        let err = stream.try_collect::<Vec<_>>().await.unwrap_err();
        assert_eq!(err.kind(), kind);
        assert_eq!(err.send_state(), UpstreamSendState::Sent);
        released(&fixture).await;
    }
}

#[tokio::test]
async fn empty_protobuf_count_response_is_an_explicit_zero() {
    let upstream = MockServer::start().await;
    Mock::given(path("/v1internal:countTokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&upstream)
        .await;
    let fixture = Fixture::new(&upstream.uri());
    let events = fixture
        .provider
        .clone()
        .execute(
            counted("genai", json!({"contents":[]})),
            context(profile(), CancellationToken::new()),
        )
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    let wire = events[0].wire_event().unwrap();
    let value: Value = serde_json::from_slice(wire.raw_json_body().unwrap()).unwrap();
    assert_eq!(value, json!({"totalTokens":0}));
    released(&fixture).await;
}
