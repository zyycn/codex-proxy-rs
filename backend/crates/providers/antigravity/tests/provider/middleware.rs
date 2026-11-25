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
