//! Attempt 中间件业务头不得替换认证或冻结身份，也不能使上游业务提前发送

use bytes::Bytes;
use futures::{StreamExt, TryStreamExt, future::BoxFuture};
use gateway_core::{
    engine::{
        ModelRequestId, RequestAttemptContext,
        execution::ClientTransport,
        middleware::{
            FrozenMiddlewarePlan, MiddlewareContext, MiddlewareError, MiddlewareHeader,
            MiddlewareNext, MiddlewarePlan, MiddlewareRequest, MiddlewareResponse,
        },
        provider::Provider,
    },
    error::ProviderErrorKind,
    lifecycle::CancellationToken,
    policy::ClientApiKeyId,
    routing::extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference},
    upstream::UpstreamSendState,
};
use serde_json::json;
use std::sync::Arc;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, path},
};

use crate::{
    profile::{self, ReleaseService, Selection},
    support::{Fixture, context_with_request, request},
};

#[derive(Debug)]
struct Headers;

impl ExtensionSetLease for Headers {
    fn is_ready(&self) -> bool {
        true
    }
}

impl MiddlewarePlan for Headers {
    fn handle(
        &self,
        _context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        let protocol = request.protocol().to_owned();
        let body = request.body().clone();
        let headers = vec![
            MiddlewareHeader::new("x-fixture-business", Bytes::from_static(b"preserved")),
            MiddlewareHeader::new("Authorization", Bytes::from_static(b"Bearer forbidden")),
            MiddlewareHeader::new("User-Agent", Bytes::from_static(b"forbidden-agent")),
        ];
        next.run(
            request
                .replace_parts(protocol, headers, body, None)
                .unwrap(),
        )
    }
}

#[tokio::test]
async fn middleware_business_headers_survive_without_overriding_frozen_authentication() {
    let upstream = MockServer::start().await;
    let fixture = Fixture::new(&upstream.uri());
    let profile = profile::resolve(
        &Selection::default().configuration(),
        &ReleaseService::new(crate::support::profile::cache()).unwrap(),
    )
    .unwrap();
    Mock::given(path("/v1internal:generateContent"))
        .and(header("x-fixture-business", "preserved"))
        .and(header("authorization", "Bearer fixture-access"))
        .and(header("user-agent", profile.user_agent().as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"response":{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP"}]}})))
        .expect(1)
        .mount(&upstream).await;
    let middleware = Arc::new(Headers);
    let plan = FrozenMiddlewarePlan::new(
        middleware.clone(),
        ExtensionSetReference::new(ExtensionSetId::new("fixture".into()).unwrap(), middleware),
    );
    let request_context = RequestAttemptContext::new(
        ModelRequestId::new("req_fixture").unwrap(),
        ClientApiKeyId::new("key_fixture").unwrap(),
    )
    .with_request_profile(Some(profile.frozen()))
    .with_middleware(
        Some(plan),
        Arc::from([]),
        "/v1beta/models/fixture-model:generateContent".into(),
        ClientTransport::HttpSse,
    );
    let stream = fixture
        .provider
        .clone()
        .execute(
            request(false),
            context_with_request(request_context.clone(), CancellationToken::new()),
        )
        .await
        .unwrap();
    assert!(upstream.received_requests().await.unwrap().is_empty());
    drop(stream);
    assert!(upstream.received_requests().await.unwrap().is_empty());
    let cancellation = CancellationToken::new();
    let mut stream = fixture
        .provider
        .clone()
        .execute(
            request(false),
            context_with_request(request_context.clone(), cancellation.clone()),
        )
        .await
        .unwrap();
    cancellation.cancel();
    let error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::Cancelled);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    drop(stream);
    assert!(upstream.received_requests().await.unwrap().is_empty());
    let stream = fixture
        .provider
        .clone()
        .execute(
            request(false),
            context_with_request(request_context, CancellationToken::new()),
        )
        .await
        .unwrap();
    let events: Vec<_> = stream.try_collect().await.unwrap();
    assert!(events.iter().any(|event| event.has_client_event()));
}

#[derive(Debug)]
struct ProtocolRewrite {
    protocol: &'static str,
    body: Bytes,
}

impl ExtensionSetLease for ProtocolRewrite {
    fn is_ready(&self) -> bool {
        true
    }
}

impl MiddlewarePlan for ProtocolRewrite {
    fn handle(
        &self,
        _: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        let headers = request.headers().to_vec();
        next.run(
            request
                .replace_parts(self.protocol.to_owned(), headers, self.body.clone(), None)
                .unwrap(),
        )
    }
}

fn protocol_context(protocol: &'static str, body: Bytes) -> gateway_core::engine::AttemptContext {
    let middleware = Arc::new(ProtocolRewrite { protocol, body });
    let plan = FrozenMiddlewarePlan::new(
        middleware.clone(),
        ExtensionSetReference::new(
            ExtensionSetId::new("fixture_protocol".into()).unwrap(),
            middleware,
        ),
    );
    context_with_request(
        RequestAttemptContext::new(
            ModelRequestId::new("req_plugin_protocol").unwrap(),
            ClientApiKeyId::new("key_fixture").unwrap(),
        )
        .with_middleware(
            Some(plan),
            Arc::from([]),
            "/v1/chat/completions".into(),
            ClientTransport::HttpJson,
        ),
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn explicit_native_rewrites_run_without_borrowing_the_builtin_chat_converter() {
    use gateway_core::{
        engine::provider::ProviderRequest,
        operation::{GenerateRequest, Operation, ProtocolPayload},
    };
    let upstream = MockServer::start().await;
    let fixture = Fixture::new(&upstream.uri());
    Mock::given(path("/v1internal:generateContent"))
        .and(wiremock::matchers::body_partial_json(json!({"request":{"contents":[{"role":"user","parts":[{"text":"plugin converted"}]}]}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"response":{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP"}]}}))).expect(2).mount(&upstream).await;
    for in_attempt in [false, true] {
        let source = Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                "chat_completions",
                json!({"model":"fixture-model","messages":[{"role":"user","content":"hello"}]})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap(),
        ));
        let native = Bytes::from_static(
            br#"{"contents":[{"role":"user","parts":[{"text":"plugin converted"}]}]}"#,
        );
        let operation = if in_attempt {
            source
        } else {
            source
                .replace_middleware_wire("genai", native.clone())
                .unwrap()
        };
        let mut stream = fixture
            .provider
            .clone()
            .execute(
                ProviderRequest::new(operation, request(false).candidate().clone()),
                protocol_context("genai", native),
            )
            .await
            .unwrap();
        assert_eq!(fixture.store.active_leases(), 1);
        let mut delivered = false;
        while let Some(event) = stream.next().await {
            let event = event.unwrap();
            let terminal = event
                .canonical_facts()
                .iter()
                .any(|fact| matches!(fact, gateway_core::event::GatewayEvent::Completed(_)));
            for event in stream.translate_native_response(event, terminal).unwrap() {
                if let Some(wire) = event.wire_event() {
                    let body: serde_json::Value =
                        serde_json::from_slice(wire.raw_json_body().unwrap()).unwrap();
                    assert!(body.get("choices").is_none());
                    assert_eq!(body["candidates"][0]["content"]["parts"][0]["text"], "ok");
                    delivered = true;
                }
            }
        }
        assert!(delivered);
        drop(stream);
        assert_eq!(fixture.store.active_leases(), 0);
    }
    upstream.verify().await;
}

#[tokio::test]
async fn explicit_middleware_cannot_relabel_native_genai_as_an_authorized_chat_source() {
    let upstream = MockServer::start().await;
    let fixture = Fixture::new(&upstream.uri());
    let error = fixture
        .provider
        .clone()
        .execute(
            request(false),
            protocol_context(
                "chat_completions",
                Bytes::from_static(
                    br#"{"model":"fixture-model","messages":[{"role":"user","content":"hello"}]}"#,
                ),
            ),
        )
        .await
        .err()
        .expect("a native source cannot borrow the built-in Chat converter");
    assert_eq!(error.kind(), ProviderErrorKind::Unsupported);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(upstream.received_requests().await.unwrap().is_empty());
    assert_eq!(fixture.store.active_leases(), 0);
}
