//! 出站准备链的返回顺序、预算、生命周期与不可修改归属

use super::*;
use gateway_core::account::{AccountSelectionPolicy, RotationStrategy};
use gateway_core::engine::middleware::outbound::{Pipeline, Request, Target};
use gateway_core::engine::{AccountAttemptContext, AttemptContext, RequestAttemptContext};
use gateway_core::error::ProviderErrorKind;
use gateway_core::upstream::UpstreamSendState;

#[derive(Debug, Clone, Copy)]
enum Action {
    Forward,
    Rewrite,
    Oversize,
    SwitchTransport,
    Reject,
    Pending,
    Cancel,
}

#[derive(Debug)]
struct PreparingPlan {
    action: Action,
    calls: Arc<AtomicUsize>,
}

impl MiddlewarePlan for PreparingPlan {
    fn has_outbound(&self, context: &MiddlewareContext) -> bool {
        context.model() == Some("gpt-test")
    }

    fn handle_outbound(
        &self,
        context: MiddlewareContext,
        request: Request,
        next: gateway_core::engine::middleware::outbound::Next,
    ) -> BoxFuture<'static, Result<Request, MiddlewareError>> {
        assert_eq!(context.mount(), MiddlewareMount::Outbound);
        assert_eq!(context.provider().unwrap().as_str(), "openai");
        assert_eq!(context.account_id().unwrap().as_str(), "acct_middleware");
        assert_eq!(context.model(), Some("gpt-test"));
        assert_eq!(context.attempt_index(), NonZeroU32::new(2));
        assert_eq!(context.request_id().as_str(), "req_outbound");
        assert_eq!(context.client_key_id().as_str(), "key_outbound");
        assert!(context.plan().is_some());
        self.calls.fetch_add(1, Ordering::SeqCst);
        let action = self.action;
        Box::pin(async move {
            let mut request = next.run(request).await?;
            match action {
                Action::Forward => (),
                Action::Rewrite => {
                    let mut body = request.body.to_vec();
                    body.extend_from_slice(b"-after-next");
                    request.body = body.into();
                    request
                        .headers
                        .push(MiddlewareHeader::new("x-raw", Bytes::from_static(&[0x80])));
                }
                Action::Oversize => request.body = Bytes::from_static(b"too large"),
                Action::SwitchTransport => {
                    request.target = Target::WebSocket {
                        uri: "wss://example.test/".parse().unwrap(),
                    }
                }
                Action::Reject => return Err(MiddlewareError::Rejected),
                Action::Pending => return futures::future::pending().await,
                Action::Cancel => {
                    context.cancellation().cancel();
                    return futures::future::pending().await;
                }
            }
            Ok(request)
        })
    }

    fn handle(
        &self,
        _: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        next.run(request)
    }
}

fn pipeline(
    action: Action,
    limit: usize,
    cancellation: CancellationToken,
    deadline: SystemTime,
) -> (Pipeline, Arc<AtomicUsize>) {
    let (attempt, calls) = attempt(action, limit, cancellation, deadline);
    (
        attempt
            .outbound_middleware(
                ProviderKind::new("openai").unwrap(),
                Some("gpt-test".into()),
                ProviderAccountId::new("acct_middleware").unwrap(),
            )
            .unwrap(),
        calls,
    )
}

fn attempt(
    action: Action,
    limit: usize,
    cancellation: CancellationToken,
    deadline: SystemTime,
) -> (AttemptContext, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let plan = FrozenMiddlewarePlan::new(
        Arc::new(PreparingPlan {
            action,
            calls: Arc::clone(&calls),
        }),
        ExtensionSetReference::new(
            ExtensionSetId::new("outbound-test".into()).unwrap(),
            Arc::new(ReadyGeneration),
        ),
    );
    let request = RequestAttemptContext::new(
        ModelRequestId::new("req_outbound").unwrap(),
        ClientApiKeyId::new("key_outbound").unwrap(),
    )
    .with_request_body_limit(limit)
    .with_middleware(
        Some(plan),
        Arc::from([]),
        "/v1/responses".into(),
        ClientTransport::HttpJson,
    );
    let attempt = AttemptContext::new(
        request,
        NonZeroU32::new(2).unwrap(),
        deadline,
        AccountSelectionPolicy::new(RotationStrategy::Smart, NonZeroU32::new(1).unwrap()),
        AccountAttemptContext::new(Default::default(), None, None),
        None,
        cancellation,
    );
    (attempt, calls)
}

fn request() -> Request {
    Request {
        target: Target::Http {
            method: http::Method::POST,
            uri: "https://example.test/path?secret=private".parse().unwrap(),
        },
        headers: vec![MiddlewareHeader::new("x-raw", Bytes::from_static(b"first"))],
        body: Bytes::from_static(&[0, 255, 0, 128]),
    }
}

#[test]
fn outbound_returns_outer_rewrite_after_next_without_decoding_body() {
    let (pipeline, calls) = pipeline(
        Action::Rewrite,
        128,
        CancellationToken::new(),
        SystemTime::now() + Duration::from_secs(1),
    );
    let prepared = futures::executor::block_on(pipeline.prepare(request())).unwrap();
    assert_eq!(
        prepared.body.as_ref(),
        &[
            0, 255, 0, 128, b'-', b'a', b'f', b't', b'e', b'r', b'-', b'n', b'e', b'x', b't'
        ]
    );
    let headers = prepared.header_map().unwrap();
    assert_eq!(
        headers
            .get_all("x-raw")
            .iter()
            .map(http::HeaderValue::as_bytes)
            .collect::<Vec<_>>(),
        vec![b"first".as_slice(), &[0x80]]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn outbound_checks_final_body_limit_without_repair_or_retry() {
    let (pipeline, _) = pipeline(
        Action::Oversize,
        4,
        CancellationToken::new(),
        SystemTime::now() + Duration::from_secs(1),
    );
    let error = futures::executor::block_on(pipeline.prepare(request())).unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(error.retry_is_prohibited());
}

#[test]
fn outbound_cannot_switch_transport_kind() {
    let (pipeline, _) = pipeline(
        Action::SwitchTransport,
        128,
        CancellationToken::new(),
        SystemTime::now() + Duration::from_secs(1),
    );
    let error = futures::executor::block_on(pipeline.prepare(request())).unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert!(error.retry_is_prohibited());
}

#[test]
fn outbound_rejection_is_terminal_and_not_sent() {
    let (pipeline, _) = pipeline(
        Action::Reject,
        128,
        CancellationToken::new(),
        SystemTime::now() + Duration::from_secs(1),
    );
    let error = futures::executor::block_on(pipeline.prepare(request())).unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::RequestPolicyDenied);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(error.retry_is_prohibited());
}

#[test]
fn outbound_cancelled_attempt_never_enters_middleware() {
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let (pipeline, calls) = pipeline(
        Action::Forward,
        128,
        cancelled,
        SystemTime::now() + Duration::from_secs(1),
    );
    let error = futures::executor::block_on(pipeline.prepare(request())).unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn outbound_deadline_drops_a_pending_preparation() {
    let (pipeline, calls) = pipeline(
        Action::Pending,
        128,
        CancellationToken::new(),
        SystemTime::now() + Duration::from_secs(1),
    );
    let error = futures::executor::block_on(pipeline.prepare(request())).unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::Timeout);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(error.retry_is_prohibited());
}

#[test]
fn outbound_header_conversion_rejects_invalid_bytes_without_retry() {
    let mut request = request();
    request.headers.push(MiddlewareHeader::new(
        "x-invalid",
        Bytes::from_static(b"bad\r\nvalue"),
    ));
    let error = request.header_map().unwrap_err();
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    assert!(error.retry_is_prohibited());
}

#[test]
fn outbound_debug_does_not_expose_uri_headers_or_body() {
    let debug = format!("{:?}", request());
    assert!(!debug.contains("private"));
    assert!(!debug.contains("first"));
    assert!(!debug.contains("255"));
}

#[test]
fn outbound_cancellation_stops_an_in_flight_preparation() {
    let (pipeline, calls) = pipeline(
        Action::Cancel,
        128,
        CancellationToken::new(),
        SystemTime::now() + Duration::from_secs(1),
    );
    let error = futures::executor::block_on(pipeline.prepare(request())).unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(error.retry_is_prohibited());
}

#[test]
fn outbound_scope_miss_keeps_the_provider_on_its_native_path() {
    let (attempt, calls) = attempt(
        Action::Reject,
        0,
        CancellationToken::new(),
        SystemTime::now() + Duration::from_secs(1),
    );
    let pipeline = attempt.outbound_middleware(
        ProviderKind::new("openai").unwrap(),
        Some("unmatched".into()),
        ProviderAccountId::new("acct_middleware").unwrap(),
    );
    assert!(pipeline.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
