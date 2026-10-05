//! HTTP 与 WebSocket 中间件测试入口及请求生命周期替身

mod websocket;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use bytes::Bytes;
use futures::{future::BoxFuture, stream};
use gateway_core::{
    engine::middleware::{
        FrozenMiddlewarePlan, MiddlewareContext, MiddlewareError, MiddlewareNext, MiddlewarePlan,
        MiddlewareRequest, MiddlewareResponse,
    },
    lifecycle::CancellationToken,
    middleware::http as contract,
    runtime::extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference},
};
use http_body::Frame;
use http_body_util::{BodyExt as _, StreamBody};
use tower::ServiceExt as _;

#[derive(Clone, Copy, Debug)]
enum Mode {
    Rewrite,
    Upgrade {
        reject: bool,
        idle: bool,
        block_messages: bool,
    },
    Service,
    Echo,
    Slow {
        clear_timeout: bool,
    },
}

#[derive(Debug)]
struct Plan {
    mode: Mode,
    calls: Arc<AtomicUsize>,
    cancellation: Arc<Mutex<Option<CancellationToken>>>,
    message_lifetime: Arc<(tokio::sync::Notify, tokio::sync::Notify)>,
}
struct Lease;
impl ExtensionSetLease for Lease {
    fn is_ready(&self) -> bool {
        true
    }
}

impl MiddlewarePlan for Plan {
    fn has_http(&self) -> bool {
        !matches!(self.mode, Mode::Service)
    }
    fn has_service(&self) -> bool {
        matches!(self.mode, Mode::Service)
    }
    fn has_websocket(&self) -> bool {
        matches!(self.mode, Mode::Upgrade { .. })
    }
    fn handle_websocket(
        &self,
        context: gateway_core::middleware::websocket::Context,
        message: gateway_core::middleware::websocket::Message,
        next: gateway_core::middleware::websocket::Next,
    ) -> BoxFuture<
        'static,
        Result<Option<gateway_core::middleware::websocket::Message>, MiddlewareError>,
    > {
        assert!(context.plan.is_some());
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !matches!(
            self.mode,
            Mode::Upgrade {
                block_messages: true,
                ..
            }
        ) {
            return next.run(message);
        }
        let lifetime = self.message_lifetime.clone();
        Box::pin(async move {
            struct NotifyOnDrop(Arc<(tokio::sync::Notify, tokio::sync::Notify)>);
            impl Drop for NotifyOnDrop {
                fn drop(&mut self) {
                    self.0.1.notify_one();
                }
            }
            let _guard = NotifyOnDrop(lifetime.clone());
            lifetime.0.notify_one();
            std::future::pending().await
        })
    }
    fn handle_service(
        &self,
        context: gateway_core::middleware::service::Context,
        input: serde_json::Value,
        next: gateway_core::middleware::service::Next,
    ) -> BoxFuture<'static, Result<serde_json::Value, gateway_core::middleware::service::Error>>
    {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(context.operation, "settings.load");
        assert_eq!(context.request_id, "service-request");
        assert_eq!(context.parent_call_id.as_deref(), Some("service-request"));
        assert!(input.is_null());
        Box::pin(async move {
            let mut result = next.run(input).await?;
            result["request_interval_ms"] = serde_json::json!(4321);
            Ok(result)
        })
    }
    fn handle_http(
        &self,
        context: contract::Context,
        mut request: contract::Request,
        next: contract::Next,
    ) -> BoxFuture<'static, Result<contract::Response, MiddlewareError>> {
        if matches!(self.mode, Mode::Service) {
            return next.run(request);
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.cancellation.lock().unwrap() = Some(context.cancellation);
        let mode = self.mode;
        Box::pin(async move {
            match mode {
                Mode::Service => unreachable!("service-only plan bypasses HTTP layers"),
                Mode::Upgrade { reject, idle, .. } => {
                    let upgrade = request
                        .extensions_mut()
                        .remove::<Arc<dyn contract::upgrade::WebSocketUpgrade>>()
                        .unwrap();
                    let (parts, _) = request.into_parts();
                    let (mut response, session) =
                        upgrade.accept(parts, vec!["test-session".into()]).await?;
                    let status = response.status();
                    response.extensions_mut().insert(contract::upgrade::Upgraded::new(status, Box::pin(async move {
                        let session = session.await.map_err(|_| MiddlewareError::Fault)?;
                        if idle { return std::future::pending().await; }
                        use gateway_core::middleware::websocket::{Kind, Message};
                        let receive = session.receive();
                        tokio::pin!(receive);
                        tokio::select! {
                            message = &mut receive => panic!("must await the client: {}", message.is_ok()),
                            () = tokio::time::sleep(Duration::from_millis(30)) => {},
                        }
                        session.send(Message { kind: Kind::Text, payload: Bytes::from_static(b"ready") }).await?;
                        let message = receive.await?.unwrap();
                        session.send(message).await?;
                        session.send(Message { kind: Kind::Close { code: Some(1000) }, payload: Bytes::from_static(b"done") }).await
                    })));
                    if reject {
                        *response.status_mut() = StatusCode::FORBIDDEN;
                    }
                    Ok(response)
                }
                Mode::Rewrite => {
                    assert_eq!(request.uri(), "/unrecognized/client/path?raw=%2f%FF");
                    assert_eq!(request.headers()["authorization"], "Bearer fixture-secret");
                    assert_eq!(request.headers().get_all("x-repeat").iter().count(), 2);
                    *request.uri_mut() = "/healthz".parse().unwrap();
                    *request.method_mut() = http::Method::GET;
                    next.run(request).await
                }
                Mode::Echo => {
                    let (parts, body) = request.into_parts();
                    let mut response = contract::Response::new(body);
                    *response.status_mut() = StatusCode::CREATED;
                    *response.headers_mut() = parts.headers;
                    response.extensions_mut().insert(123_u32);
                    Ok(response)
                }
                Mode::Slow { clear_timeout } => {
                    assert_eq!(
                        request
                            .extensions()
                            .get::<contract::Settings>()
                            .unwrap()
                            .timeout,
                        Some(Duration::from_secs(1))
                    );
                    if clear_timeout {
                        request
                            .extensions_mut()
                            .get_mut::<contract::Settings>()
                            .unwrap()
                            .timeout = None;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    next.run(request).await
                }
            }
        })
    }
    fn handle(
        &self,
        _: MiddlewareContext,
        _: MiddlewareRequest,
        _: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        Box::pin(async { panic!("HTTP 入口不能伪装为已认证模型请求") })
    }
}

async fn router(mode: Mode, timeout: Option<u64>) -> (axum::Router, Arc<Plan>) {
    let admin = crate::admin::AdminTestFixture::new().await;
    admin.auth.insert_session("service-fixture");
    let plan = Arc::new(Plan {
        mode,
        calls: Arc::default(),
        cancellation: Arc::default(),
        message_lifetime: Arc::default(),
    });
    let frozen = FrozenMiddlewarePlan::new(
        plan.clone(),
        ExtensionSetReference::new(
            ExtensionSetId::new("http-test".into()).unwrap(),
            Arc::new(Lease),
        ),
    );
    let router = crate::openai::api_bundle(
        admin.services,
        gateway_api::ApiConfig {
            asset_directory: std::env::temp_dir(),
            cors_allowed_origins: Vec::new(),
            request_timeout_seconds: timeout,
            request_id_header: "x-request-id".into(),
        },
    )
    .with_middleware(move |_| Some(frozen.clone()))
    .router();
    (router, plan)
}

#[tokio::test]
async fn rewrites_unknown_path_before_routing_without_reading_request_body() {
    let (router, plan) = router(Mode::Rewrite, None).await;
    let reads = Arc::new(AtomicUsize::new(0));
    let body_reads = reads.clone();
    let body = Body::from_stream(stream::poll_fn(move |_| {
        body_reads.fetch_add(1, Ordering::SeqCst);
        std::task::Poll::Ready(Some(Ok::<_, std::io::Error>(Bytes::from_static(
            b"must remain lazy",
        ))))
    }));
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/unrecognized/client/path?raw=%2f%FF")
                .header("authorization", "Bearer fixture-secret")
                .header("x-repeat", "one")
                .header("x-repeat", "two")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(plan.calls.load(Ordering::SeqCst), 1);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    assert!(
        !plan
            .cancellation
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled()
    );
    drop(response);
    assert!(
        plan.cancellation
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled()
    );
}

#[tokio::test]
async fn short_circuit_preserves_binary_headers_trailers_extensions_and_backpressure() {
    let (router, _) = router(Mode::Echo, None).await;
    let mut trailers = http::HeaderMap::new();
    trailers.append("x-end", "one".parse().unwrap());
    trailers.append("x-end", "two".parse().unwrap());
    let reads = Arc::new(AtomicUsize::new(0));
    let observed = reads.clone();
    let mut frames = vec![
        Frame::data(Bytes::from_static(b"first")),
        Frame::data(Bytes::from_static(b"second")),
        Frame::trailers(trailers),
    ]
    .into_iter();
    let body = Body::new(StreamBody::new(stream::poll_fn(move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        std::task::Poll::Ready(frames.next().map(Ok::<_, std::io::Error>))
    })));
    let response = router
        .oneshot(
            Request::builder()
                .uri("/arbitrary/plugin/route")
                .header("x-binary", http::HeaderValue::from_bytes(&[0xFF]).unwrap())
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()["x-binary"].as_bytes(), &[0xFF]);
    assert_eq!(response.extensions().get::<u32>(), Some(&123));
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let mut body = response.into_body();
    assert_eq!(
        body.frame().await.unwrap().unwrap().into_data().unwrap(),
        "first"
    );
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        body.frame().await.unwrap().unwrap().into_data().unwrap(),
        "second"
    );
    let end = body
        .frame()
        .await
        .unwrap()
        .unwrap()
        .into_trailers()
        .unwrap();
    assert_eq!(end.get_all("x-end").iter().count(), 2);
    assert!(body.frame().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn host_timeout_is_a_baseline_and_plugin_override_is_consumed_by_terminal() {
    for clear_timeout in [false, true] {
        let (router, _) = router(Mode::Slow { clear_timeout }, Some(1)).await;
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if clear_timeout {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::REQUEST_TIMEOUT
            }
        );
    }
}

#[tokio::test]
async fn admin_http_does_not_invoke_service_middleware_for_native_business_methods() {
    let (router, plan) = router(Mode::Service, None).await;
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/admin/settings")
                .header("cookie", "cpr_session=service-fixture")
                .header("x-request-id", "service-request")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_ne!(result["data"]["requestIntervalMs"], 4321);
    assert_eq!(plan.calls.load(Ordering::SeqCst), 0);
}

#[derive(Debug)]
struct Inspect(Arc<Mutex<Vec<contract::Context>>>);
impl gateway_core::engine::middleware::MiddlewarePlan for Inspect {
    fn has_http(&self) -> bool {
        true
    }
    fn handle_http(
        &self,
        context: contract::Context,
        mut request: contract::Request,
        next: contract::Next,
    ) -> BoxFuture<'static, Result<contract::Response, MiddlewareError>> {
        self.0.lock().unwrap().push(context.clone());
        assert_eq!(
            request
                .extensions()
                .get::<contract::Context>()
                .unwrap()
                .call_id,
            context.call_id
        );
        assert!(context.extensions.contains("origin-plugin"));
        assert_eq!(request.headers()["x-request-id"], "parent-request");
        assert_eq!(
            request
                .extensions()
                .get::<contract::Settings>()
                .unwrap()
                .timeout,
            None
        );
        *request.uri_mut() = "/healthz".parse().unwrap();
        next.run(request)
    }
    fn handle(
        &self,
        _: MiddlewareContext,
        _: MiddlewareRequest,
        _: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        Box::pin(async { panic!("health request has no model boundary") })
    }
}

#[tokio::test]
async fn internal_dispatch_uses_the_same_router_and_the_frozen_parent_context() {
    let admin = crate::admin::AdminTestFixture::new().await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let frozen = FrozenMiddlewarePlan::new(
        Arc::new(Inspect(calls.clone())),
        ExtensionSetReference::new(
            ExtensionSetId::new("frozen-child".into()).unwrap(),
            Arc::new(Lease),
        ),
    );
    let bundle = crate::openai::api_bundle(
        admin.services,
        gateway_api::ApiConfig {
            asset_directory: std::env::temp_dir(),
            cors_allowed_origins: Vec::new(),
            request_timeout_seconds: Some(60),
            request_id_header: "x-request-id".into(),
        },
    );
    let dispatcher = bundle.dispatcher();
    let parent = CancellationToken::new();
    let child = parent.child_token();
    let context = contract::Context {
        plugin_instance_id: None,
        request_id: "parent-request".into(),
        call_id: "child-call".into(),
        parent_call_id: Some("parent-call".into()),
        extensions: Default::default(),
        plan: Some(frozen),
        cancellation: child.clone(),
    };
    let context = contract::Context {
        extensions: context
            .extensions
            .extending("origin-plugin".into())
            .unwrap(),
        ..context
    };
    let mut request = http::Request::builder()
        .uri("/plugin/unknown/route")
        .body(contract::empty_body())
        .unwrap();
    request.extensions_mut().insert(contract::Settings {
        timeout: None,
        runtime: None,
    });
    let response = dispatcher.dispatch(context, request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!child.is_cancelled());
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
    assert!(child.is_cancelled());
    assert!(!parent.is_cancelled());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].call_id, "child-call");
    assert_eq!(calls[0].parent_call_id.as_deref(), Some("parent-call"));
}

#[tokio::test]
async fn parent_cancellation_wakes_a_pending_child_response_body() {
    let admin = crate::admin::AdminTestFixture::new().await;
    let plan = Arc::new(Plan {
        mode: Mode::Echo,
        calls: Arc::default(),
        cancellation: Arc::default(),
        message_lifetime: Arc::default(),
    });
    let frozen = FrozenMiddlewarePlan::new(
        plan,
        ExtensionSetReference::new(
            ExtensionSetId::new("cancel-child".into()).unwrap(),
            Arc::new(Lease),
        ),
    );
    let bundle = crate::openai::api_bundle(
        admin.services,
        gateway_api::ApiConfig {
            asset_directory: std::env::temp_dir(),
            cors_allowed_origins: Vec::new(),
            request_timeout_seconds: None,
            request_id_header: "x-request-id".into(),
        },
    );
    let parent = CancellationToken::new();
    let context = contract::Context {
        plugin_instance_id: None,
        request_id: "parent".into(),
        call_id: "child".into(),
        parent_call_id: Some("parent".into()),
        extensions: Default::default(),
        plan: Some(frozen),
        cancellation: parent.child_token(),
    };
    let entered = Arc::new(tokio::sync::Notify::new());
    let observed = entered.clone();
    let body = StreamBody::new(stream::poll_fn(move |_| {
        observed.notify_one();
        std::task::Poll::Pending::<Option<Result<Frame<Bytes>, std::io::Error>>>
    }))
    .map_err(|error| Box::new(error) as _)
    .boxed_unsync();
    let response = bundle
        .dispatcher()
        .dispatch(context, contract::Request::new(body))
        .await
        .unwrap();
    let task = tokio::spawn(async move { response.into_body().collect().await });
    entered.notified().await;
    parent.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
}

#[tokio::test]
async fn plugin_http_dispatch_uses_host_identity_without_client_credentials() {
    let admin = crate::admin::AdminTestFixture::new().await;
    let bundle = crate::openai::api_bundle(
        admin.services,
        gateway_api::ApiConfig {
            asset_directory: std::env::temp_dir(),
            cors_allowed_origins: Vec::new(),
            request_timeout_seconds: None,
            request_id_header: "x-request-id".into(),
        },
    );
    let dispatcher = bundle.dispatcher();
    let context = contract::Context {
        plugin_instance_id: Some("installed-plugin".into()),
        request_id: "model-parent".into(),
        call_id: "management-child".into(),
        parent_call_id: Some("parent-call".into()),
        extensions: Default::default(),
        plan: None,
        cancellation: CancellationToken::new(),
    };
    let response = dispatcher
        .dispatch(
            context.clone(),
            http::Request::builder()
                .uri("/api/admin/settings")
                .body(contract::empty_body())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(data["data"].is_object());
    let mutation = contract::Context {
        cancellation: CancellationToken::new(),
        call_id: "mutation-child".into(),
        ..context
    };
    let response = dispatcher
        .dispatch(
            mutation,
            http::Request::builder()
                .method("POST")
                .uri("/api/admin/settings/admin-api-key/delete")
                .body(contract::empty_body())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.into_body().collect().await.unwrap();
    let response = bundle
        .router()
        .oneshot(
            Request::builder()
                .uri("/api/admin/settings")
                .header("x-plugin-instance-id", "installed-plugin")
                .header("x-parent-call-id", "parent-call")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
