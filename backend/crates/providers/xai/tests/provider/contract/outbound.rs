//! 验证 xAI 最终出站改写与真实传输、取消及账号事实的边界

use super::*;
use gateway_core::engine::middleware::outbound;
use provider_xai::ReqwestGrokInferenceTransport;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Debug)]
struct Rewrite {
    uri: String,
    calls: Arc<AtomicUsize>,
    invalid_header: bool,
}

impl MiddlewarePlan for Rewrite {
    fn has_outbound(&self, _context: &MiddlewareContext) -> bool {
        true
    }

    fn handle(
        &self,
        _: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        next.run(request)
    }

    fn handle_outbound(
        &self,
        context: MiddlewareContext,
        mut request: outbound::Request,
        next: outbound::Next,
    ) -> BoxFuture<'static, Result<outbound::Request, MiddlewareError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(context.mount(), MiddlewareMount::Outbound);
        assert!(
            request
                .headers
                .iter()
                .any(|header| header.name() == "user-agent")
        );
        assert!(serde_json::from_slice::<Value>(&request.body).is_ok());
        request.target = outbound::Target::Http {
            method: reqwest::Method::PATCH,
            uri: self.uri.parse().unwrap(),
        };
        request.headers = vec![
            MiddlewareHeader::new("x-multi", Bytes::from_static(b"one")),
            MiddlewareHeader::new("X-Multi", Bytes::from_static(b"two")),
            MiddlewareHeader::new("x-raw", Bytes::from_static(b"\x80")),
        ];
        if self.invalid_header {
            request
                .headers
                .push(MiddlewareHeader::new("invalid\nheader", Bytes::new()));
        }
        request.body = Bytes::from_static(b"\0\xffoutbound\x80");
        next.run(request)
    }
}

#[tokio::test]
async fn outbound_sends_final_method_headers_and_binary_bytes_for_generation_and_compaction() {
    for compact in [false, true] {
        let server = MockServer::start().await;
        let response = if compact {
            compaction_sse(&valid_compaction_summary("outbound compaction"), None)
        } else {
            SUCCESS_SSE.to_vec()
        };
        Mock::given(method("PATCH"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(response, "text/event-stream"))
            .expect(1)
            .mount(&server)
            .await;
        let origin = url::Url::parse(&server.uri()).unwrap();
        let transport = Arc::new(
            ReqwestGrokInferenceTransport::new(crate::support::loopback_endpoint_policy(&origin))
                .unwrap(),
        );
        let provider = provider_with_catalog_transport(
            StubSelector::success(),
            transport,
            StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
            Arc::new(StaticCatalogTransport),
        )
        .await;
        let calls = Arc::new(AtomicUsize::new(0));
        let operation = if compact {
            compaction_operation()
        } else {
            operation()
        };
        let events = provider
            .execute(
                provider_request_with_operation("xai", operation),
                context_with_middleware(Arc::new(Rewrite {
                    uri: origin.join("v1/responses").unwrap().to_string(),
                    calls: Arc::clone(&calls),
                    invalid_header: false,
                })),
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(events.iter().all(Result::is_ok), "{events:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let sent = &requests[0];
        assert_eq!(sent.body, b"\0\xffoutbound\x80");
        assert!(!sent.headers.contains_key("user-agent"));
        assert!(!sent.headers.contains_key("authorization"));
        // 底层 reqwest 的缺省 Accept 不在可禁止范围内
        assert_eq!(sent.headers["accept"], "*/*");
        assert!(!sent.headers.contains_key("content-type"));
        assert_eq!(
            sent.headers
                .get_all("x-multi")
                .iter()
                .map(|value| value.as_bytes())
                .collect::<Vec<_>>(),
            vec![b"one", b"two"]
        );
        assert_eq!(sent.headers["x-raw"].as_bytes(), b"\x80");
    }
}

#[tokio::test]
async fn invalid_outbound_header_fails_before_send_without_repair_or_retry() {
    let transport = StubInferenceTransport::success();
    let provider = provider(StubSelector::success(), transport.clone()).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut stream = Arc::clone(&provider)
        .execute(
            provider_request("xai"),
            context_with_middleware(Arc::new(Rewrite {
                uri: "https://cli-chat-proxy.grok.com/v1/responses".to_owned(),
                calls: Arc::clone(&calls),
                invalid_header: true,
            })),
        )
        .await
        .unwrap();
    let error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(error.retry_is_prohibited());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    drop(stream);
    execute_successfully(&provider, operation()).await;
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unpolled_stream_does_not_run_outbound_or_send() {
    let transport = StubInferenceTransport::success();
    let provider = provider(StubSelector::success(), transport.clone()).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let stream = Arc::clone(&provider)
        .execute(
            provider_request("xai"),
            context_with_middleware(Arc::new(Rewrite {
                uri: "https://cli-chat-proxy.grok.com/v1/responses".to_owned(),
                calls: Arc::clone(&calls),
                invalid_header: false,
            })),
        )
        .await
        .unwrap();
    drop(stream);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    execute_successfully(&provider, operation()).await;
}

#[tokio::test]
async fn outbound_target_outside_transport_policy_fails_without_sending_or_retry() {
    let server = MockServer::start().await;
    let origin = url::Url::parse(&server.uri()).unwrap();
    let provider = provider_with_catalog_transport(
        StubSelector::success(),
        Arc::new(
            ReqwestGrokInferenceTransport::new(crate::support::loopback_endpoint_policy(&origin))
                .unwrap(),
        ),
        StubRecovery::new(GrokCredentialRecoveryOutcome::Unavailable),
        Arc::new(StaticCatalogTransport),
    )
    .await;
    let stream = provider
        .execute(
            provider_request("xai"),
            context_with_middleware(Arc::new(Rewrite {
                uri: origin.join("unsupported-target").unwrap().to_string(),
                calls: Arc::new(AtomicUsize::new(0)),
                invalid_header: false,
            })),
        )
        .await
        .unwrap();
    let error = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .find_map(Result::err)
        .expect("transport policy rejection");
    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(error.retry_is_prohibited());
    assert!(server.received_requests().await.unwrap().is_empty());
}
